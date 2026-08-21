import type { WsMessage } from '../types/api';
import { getToken } from './auth';

export type WsMessageHandler = (msg: WsMessage) => void;
export type WsOpenHandler = () => void;
export type WsCloseHandler = (ev: CloseEvent) => void;
export type WsErrorHandler = (ev: Event) => void;

export interface WebSocketClientOptions {
  /** Base URL override. Defaults to current host with ws(s) protocol. */
  baseUrl?: string;
  /** Delay in ms before attempting reconnect. Doubles on each failure up to maxReconnectDelay. */
  reconnectDelay?: number;
  /** Maximum reconnect delay in ms. */
  maxReconnectDelay?: number;
  /** Set to false to disable auto-reconnect. Default true. */
  autoReconnect?: boolean;
}

export interface SeedChatMessage {
  role: 'user' | 'assistant' | 'agent' | 'tool';
  content: string;
}

export interface SendChatMessageOptions {
  content: string;
  sessionId?: string;
  temporary?: boolean;
  historySeed?: SeedChatMessage[];
  federationPeerIds?: string[];
  /** Exact server tool catalogue for deterministic acceptance/audit turns. */
  allowedTools?: string[];
  /** Ask the server to enforce one real call to every registered tool. */
  toolAudit?: boolean;
  /** "agent" (or omitted) is the unchanged default AGENTS.md-driven mode. */
  agentMode?: string;
}

const DEFAULT_RECONNECT_DELAY = 1000;
const MAX_RECONNECT_DELAY = 30000;
const WS_PROTOCOL = 'llamafarm.v1';

export class WebSocketClient {
  private ws: WebSocket | null = null;
  private currentDelay: number;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private intentionallyClosed = false;

  public onMessage: WsMessageHandler | null = null;
  public onOpen: WsOpenHandler | null = null;
  public onClose: WsCloseHandler | null = null;
  public onError: WsErrorHandler | null = null;

  private readonly baseUrl: string;
  private readonly reconnectDelay: number;
  private readonly maxReconnectDelay: number;
  private readonly autoReconnect: boolean;

  constructor(options: WebSocketClientOptions = {}) {
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    this.baseUrl =
      options.baseUrl ?? `${protocol}//${window.location.host}`;
    this.reconnectDelay = options.reconnectDelay ?? DEFAULT_RECONNECT_DELAY;
    this.maxReconnectDelay = options.maxReconnectDelay ?? MAX_RECONNECT_DELAY;
    this.autoReconnect = options.autoReconnect ?? true;
    this.currentDelay = this.reconnectDelay;
  }

  /** Open the WebSocket connection. */
  connect(): void {
    this.intentionallyClosed = false;
    this.clearReconnectTimer();

    const token = getToken();
    const url = `${this.baseUrl}/ws/chat`;
    const protocols = [WS_PROTOCOL];
    if (token) {
      protocols.push(`bearer.${token}`);
    }

    this.ws = new WebSocket(url, protocols);

    this.ws.onopen = () => {
      this.currentDelay = this.reconnectDelay;
      this.onOpen?.();
    };

    this.ws.onmessage = (ev: MessageEvent) => {
      try {
        const msg = JSON.parse(ev.data) as WsMessage;
        this.onMessage?.(msg);
      } catch {
        // Ignore non-JSON frames
      }
    };

    this.ws.onclose = (ev: CloseEvent) => {
      this.onClose?.(ev);
      this.scheduleReconnect();
    };

    this.ws.onerror = (ev: Event) => {
      this.onError?.(ev);
    };
  }

  /** Send a chat message to the agent. */
  sendMessage(options: SendChatMessageOptions): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new Error('WebSocket is not connected');
    }
    this.ws.send(
      JSON.stringify({
        type: 'message',
        content: options.content,
        session_id: options.sessionId,
        temporary: options.temporary,
        history_seed: options.historySeed,
        federation_peer_ids: options.federationPeerIds,
        allowed_tools: options.allowedTools,
        tool_audit: options.toolAudit,
        agent_mode: options.agentMode,
      }),
    );
  }

  /** Delete an existing chat session from the backend session store. */
  deleteSession(sessionId: string): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      return;
    }

    this.ws.send(
      JSON.stringify({
        type: 'session_delete',
        session_id: sessionId,
      }),
    );
  }

  /**
   * Request a clean cancellation of the active agent turn for one chat.
   * The gateway keeps the socket open, interrupts model/tool execution, and
   * returns a terminal `cancelled` event once the turn has stopped.
   */
  cancelSession(sessionId: string): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      return;
    }

    this.ws.send(
      JSON.stringify({
        type: 'cancel',
        session_id: sessionId,
      }),
    );
  }

  /** Close the connection without auto-reconnecting. */
  disconnect(): void {
    this.intentionallyClosed = true;
    this.clearReconnectTimer();
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
  }

  /** Returns true if the socket is open. */
  get connected(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
  }

  // ---------------------------------------------------------------------------
  // Reconnection logic
  // ---------------------------------------------------------------------------

  private scheduleReconnect(): void {
    if (this.intentionallyClosed || !this.autoReconnect) return;

    this.reconnectTimer = setTimeout(() => {
      this.currentDelay = Math.min(this.currentDelay * 2, this.maxReconnectDelay);
      this.connect();
    }, this.currentDelay);
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }
}
