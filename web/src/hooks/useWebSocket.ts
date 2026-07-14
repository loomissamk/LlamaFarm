import { useState, useEffect, useRef, useCallback } from 'react';
import { WebSocketClient, type WebSocketClientOptions } from '../lib/ws';
import type { WsMessage } from '../types/api';

export type ConnectionStatus = 'disconnected' | 'connecting' | 'connected';

export interface UseWebSocketResult {
  /** Send a chat message to the agent. */
  sendMessage: (content: string) => void;
  /** Array of all messages received during this session. */
  messages: WsMessage[];
  /** Current connection status. */
  status: ConnectionStatus;
  /** Manually connect (called automatically on mount). */
  connect: () => void;
  /** Manually disconnect. */
  disconnect: () => void;
  /** Clear the message history. */
  clearMessages: () => void;
}

export interface UseWebSocketOptions extends WebSocketClientOptions {
  /** If false, do not connect automatically on mount. Default true. */
  autoConnect?: boolean;
}

/**
 * React hook that wraps the WebSocketClient for agent chat.
 *
 * Connects on mount (unless `autoConnect` is false), accumulates incoming
 * messages, and cleans up on unmount.
 */
export function useWebSocket(
  options: UseWebSocketOptions = {},
): UseWebSocketResult {
  const { autoConnect = true, ...wsOptions } = options;

  const clientRef = useRef<WebSocketClient | null>(null);
  const [status, setStatus] = useState<ConnectionStatus>('disconnected');
  const [messages, setMessages] = useState<WsMessage[]>([]);

  // TPS tracking — all mutable state in refs so counting never causes re-renders.
  const streamStartRef = useRef<number | null>(null);
  const charCountRef = useRef(0);

  // Stable reference to the client across renders
  const getClient = useCallback((): WebSocketClient => {
    if (!clientRef.current) {
      clientRef.current = new WebSocketClient(wsOptions);
    }
    return clientRef.current;
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // Setup handlers and optionally connect on mount
  useEffect(() => {
    const client = getClient();

    client.onOpen = () => {
      setStatus('connected');
    };

    client.onClose = () => {
      setStatus('disconnected');
    };

    client.onMessage = (msg: WsMessage) => {
      setMessages((prev) => [...prev, msg]);

      if (msg.type === 'chunk' && msg.content) {
        if (streamStartRef.current === null) {
          streamStartRef.current = performance.now();
          charCountRef.current = 0;
        }
        charCountRef.current += msg.content.length;
        const elapsed = (performance.now() - streamStartRef.current) / 1000;
        if (elapsed > 0.2) {
          // ~4 chars per token — close enough for a live indicator
          const tps = Math.round(charCountRef.current / 4 / elapsed);
          globalThis.dispatchEvent(new CustomEvent('lf:tps', { detail: { tps, streaming: true } }));
        }
      } else if (msg.type === 'done') {
        if (streamStartRef.current !== null) {
          const elapsed = (performance.now() - streamStartRef.current) / 1000;
          const tps = elapsed > 0 ? Math.round(charCountRef.current / 4 / elapsed) : 0;
          globalThis.dispatchEvent(new CustomEvent('lf:tps', { detail: { tps, streaming: false } }));
        }
        streamStartRef.current = null;
        charCountRef.current = 0;
      } else if (msg.type === 'message') {
        // User message sent — reset so next response starts fresh
        streamStartRef.current = null;
        charCountRef.current = 0;
      }
    };

    client.onError = () => {
      // Status will be set by onClose which fires after onError
    };

    if (autoConnect) {
      setStatus('connecting');
      client.connect();
    }

    return () => {
      client.disconnect();
      clientRef.current = null;
    };
  }, [getClient, autoConnect]);

  const connect = useCallback(() => {
    const client = getClient();
    setStatus('connecting');
    client.connect();
  }, [getClient]);

  const disconnect = useCallback(() => {
    const client = getClient();
    client.disconnect();
    setStatus('disconnected');
  }, [getClient]);

  const sendMessage = useCallback(
    (content: string) => {
      const client = getClient();
      client.sendMessage({ content });
      // Optimistically add the user message to the local list
      setMessages((prev) => [
        ...prev,
        { type: 'message', content } as WsMessage,
      ]);
    },
    [getClient],
  );

  const clearMessages = useCallback(() => {
    setMessages([]);
  }, []);

  return {
    sendMessage,
    messages,
    status,
    connect,
    disconnect,
    clearMessages,
  };
}
