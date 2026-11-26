"""Sonic listen-along relay.

Holds one shared "jam" per Plex account: the queue, which song is playing, whether
it's playing and where. Sonic apps signed in to the same Plex account share it.
Music never passes through here; each app streams from Plex itself.

Standard library only. Endpoints (all JSON, all need an X-Plex-Token header):
  GET  /jam/ping                       -> {"ok": true}
  GET  /jam/state?since=V&client=ID    -> the jam, waiting up to 25 s for a change
  POST /jam/state   {client, state}    -> replace the shared playback state
  POST /jam/member  {client, name, avatar}  -> join or refresh (send every ~20 s)
  POST /jam/leave   {client}           -> leave
"""

import json
import os
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

PORT = int(os.environ.get("JAM_PORT", "8790"))
MEMBER_TIMEOUT = 60  # seconds without a check-in before someone counts as gone
MAX_BODY = 4 * 1024 * 1024

lock = threading.Condition()
rooms = {}  # account id -> {"version", "state", "members"}
tokens = {}  # token -> (account id, checked at)


def now_ms():
    return int(time.time() * 1000)


def account_for(token):
    """The Plex account a token belongs to (checked with plex.tv, cached 10 min)."""
    if not token:
        return None
    if os.environ.get("JAM_TEST") == "1":  # local tests only: the token is the account
        return token
    cached = tokens.get(token)
    if cached and time.time() - cached[1] < 600:
        return cached[0]
    request = urllib.request.Request(
        "https://plex.tv/api/v2/user",
        headers={
            "Accept": "application/json",
            "X-Plex-Token": token,
            "X-Plex-Client-Identifier": "sonic-jam-relay",
            "X-Plex-Product": "Sonic Jam",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=8) as response:
            account = str(json.load(response).get("id") or "")
    except Exception:
        return None
    if not account:
        return None
    if len(tokens) > 1000:
        tokens.clear()
    tokens[token] = (account, time.time())
    return account


def room_for(account):
    room = rooms.get(account)
    if room is None:
        room = rooms[account] = {"version": 1, "state": None, "members": {}}
    return room


def prune(room):
    cutoff = time.time() - MEMBER_TIMEOUT
    gone = [client for client, member in room["members"].items() if member["seen"] < cutoff]
    for client in gone:
        del room["members"][client]
    if gone:
        room["version"] += 1
        if not room["members"]:
            room["state"] = None  # everyone left: the next jam starts fresh
    return bool(gone)


def snapshot(room):
    return {
        "version": room["version"],
        "server_now": now_ms(),
        "state": room["state"],
        "members": [
            {"client": client, "name": m["name"], "avatar": m["avatar"]}
            for client, m in sorted(room["members"].items(), key=lambda item: item[1]["joined"])
        ],
    }


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def reply(self, status, body=None):
        data = b"" if body is None else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def account(self):
        account = account_for(self.headers.get("X-Plex-Token", ""))
        if account is None:
            self.reply(401, {"error": "Sign in to Plex in Sonic first."})
        return account

    def body(self):
        length = int(self.headers.get("Content-Length") or 0)
        if length > MAX_BODY:
            raise ValueError("too big")
        return json.loads(self.rfile.read(length) or b"{}")

    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/jam/ping":
            return self.reply(200, {"ok": True})
        if url.path != "/jam/state":
            return self.reply(404, {"error": "not found"})
        account = self.account()
        if account is None:
            return
        query = parse_qs(url.query)
        since = int((query.get("since") or ["0"])[0] or 0)
        client = (query.get("client") or [""])[0]
        deadline = time.time() + 25
        with lock:
            room = room_for(account)
            while True:
                if client in room["members"]:
                    room["members"][client]["seen"] = time.time()
                if prune(room):
                    lock.notify_all()
                if room["version"] > since or time.time() >= deadline:
                    break
                lock.wait(timeout=min(5, max(0.1, deadline - time.time())))
            result = snapshot(room)
        self.reply(200, result)

    def do_POST(self):
        url = urlparse(self.path)
        account = self.account()
        if account is None:
            return
        try:
            body = self.body()
        except Exception:
            return self.reply(400, {"error": "bad request"})
        client = str(body.get("client") or "")[:64]
        if not client:
            return self.reply(400, {"error": "missing client"})
        with lock:
            room = room_for(account)
            if url.path == "/jam/member":
                member = room["members"].get(client)
                avatar = str(body.get("avatar") or "")[:200_000]
                name = str(body.get("name") or "Listener")[:40]
                if member is None:
                    room["members"][client] = {"name": name, "avatar": avatar, "seen": time.time(), "joined": time.time()}
                    room["version"] += 1
                else:
                    changed = member["name"] != name or member["avatar"] != avatar
                    member.update(name=name, avatar=avatar, seen=time.time())
                    if changed:
                        room["version"] += 1
            elif url.path == "/jam/leave":
                if room["members"].pop(client, None) is not None:
                    room["version"] += 1
                    if not room["members"]:
                        room["state"] = None
            elif url.path == "/jam/state":
                state = body.get("state")
                if not isinstance(state, dict):
                    return self.reply(400, {"error": "missing state"})
                state["by"] = client
                state["position_at"] = now_ms()
                room["state"] = state
                room["version"] += 1
                if client in room["members"]:
                    room["members"][client]["seen"] = time.time()
            else:
                return self.reply(404, {"error": "not found"})
            lock.notify_all()
            result = snapshot(room)
        self.reply(200, result)


if __name__ == "__main__":
    server = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    server.daemon_threads = True
    print(f"Sonic jam relay on port {PORT}", flush=True)
    server.serve_forever()
