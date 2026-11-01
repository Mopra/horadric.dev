#!/usr/bin/env python3
# A stand in for `claude -p --input-format stream-json`, for the Mac
# smoke run, which has no Claude to sign in to. It answers Horadric's
# control requests, greets once `initialize` comes, asks to edit a file
# so the Folio shows an ask with its diff, and answers each prompt by
# saying it back. Whatever flags it is given, it ignores.
import json
import os
import sys

SESSION = "fake-session-0001"


def say(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def init():
    say({
        "type": "system",
        "subtype": "init",
        "session_id": SESSION,
        "cwd": os.getcwd(),
        "model": "fake-model",
        "tools": ["Edit", "Bash"],
        "slash_commands": ["compact", "review"],
        "permissionMode": "default",
        "mcp_servers": [],
        "apiKeySource": "none",
    })


def reply(text, n):
    say({
        "type": "assistant",
        "session_id": SESSION,
        "message": {
            "id": "msg-%d" % n,
            "type": "message",
            "role": "assistant",
            "model": "fake-model",
            "content": [{"type": "text", "text": text}],
        },
    })


def result(text):
    say({
        "type": "result",
        "subtype": "success",
        "is_error": False,
        "duration_ms": 12,
        "num_turns": 1,
        "result": text,
        "session_id": SESSION,
        "usage": {"input_tokens": 10, "output_tokens": 5},
    })


def answer(request_id, response):
    say({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id, "response": response},
    })


def main():
    turns = 0
    greeted = False
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        kind = msg.get("type")
        if kind == "control_request":
            rid = msg.get("request_id", "")
            sub = msg.get("request", {}).get("subtype")
            if sub == "initialize":
                answer(rid, {"commands": [], "models": [], "output_style": "default"})
                if not greeted:
                    greeted = True
                    init()
                    turns += 1
                    reply("FAKE-CLAUDE-HELLO from a stand in for claude.", turns)
                    result("FAKE-CLAUDE-HELLO")
                    say({
                        "type": "control_request",
                        "request_id": "fake-ask-1",
                        "request": {
                            "subtype": "can_use_tool",
                            "tool_name": "Edit",
                            "tool_use_id": "fake-tool-1",
                            "input": {
                                "file_path": os.path.join(os.getcwd(), "notes.txt"),
                                "old_string": "first line\nold line",
                                "new_string": "first line\nnew line",
                            },
                        },
                    })
            else:
                answer(rid, {})
        elif kind == "control_response":
            # The human answered the edit ask: the turn it was in ends.
            result("FAKE-CLAUDE-ANSWERED")
        elif kind == "user":
            content = msg.get("message", {}).get("content")
            if isinstance(content, list):
                text = " ".join(c.get("text", "") for c in content if isinstance(c, dict))
            else:
                text = str(content or "")
            turns += 1
            reply("FAKE-CLAUDE-HEARD: " + text.strip(), turns)
            result("FAKE-CLAUDE-HEARD")


if __name__ == "__main__":
    main()
