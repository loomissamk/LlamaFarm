#!/usr/bin/env python3
"""Run a bounded task through the real MCP stdio server; print JSON evidence."""
import argparse
import asyncio
import json
import os
import sys
import time
from pathlib import Path

from mcp import ClientSession
from mcp.client.stdio import StdioServerParameters, stdio_client


async def run(args):
    root = Path(__file__).resolve().parent
    task = json.loads(Path(args.task).read_text())
    env = dict(os.environ, LLAMAFARM_MODEL=args.model, LLAMAFARM_TIMEOUT="240")
    local_python = root / ".venv/bin/python"
    params = StdioServerParameters(command=str(local_python) if local_python.is_file() else sys.executable,
                                  args=[str(root / "server.py")], env=env)
    started = time.monotonic()
    async with asyncio.timeout(270):
        async with stdio_client(params) as (reader, writer):
            async with ClientSession(reader, writer, read_timeout_seconds=250) as session:
                await session.initialize()
                result = await session.call_tool(args.tool, task)
                evidence = {"model_requested": args.model,
                            "elapsed_seconds": round(time.monotonic() - started, 3),
                            "result": result.model_dump(mode="json")}
                print(json.dumps(evidence, indent=2))
                if args.output:
                    Path(args.output).write_text(json.dumps(evidence, indent=2) + "\n")
                if result.is_error:
                    raise RuntimeError("MCP task failed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("task")
    parser.add_argument("--model", default="qwen3.5:9b")
    parser.add_argument("--tool", default="llamafarm_chat")
    parser.add_argument("--output", help="Save raw MCP evidence to this file")
    asyncio.run(run(parser.parse_args()))
