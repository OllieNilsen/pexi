#!/usr/bin/env python3
import base64
import json
import os
import socket
import struct
import sys
import time

HOST_CID = int(os.getenv("PEP_VSOCK_CID", "2"))
PORT = int(os.getenv("PEP_VSOCK_PORT", "4040"))


def read_frame(sock):
    header = sock.recv(4)
    if len(header) < 4:
        raise RuntimeError("short read on length header")
    (length,) = struct.unpack(">I", header)
    data = b""
    while len(data) < length:
        chunk = sock.recv(length - len(data))
        if not chunk:
            raise RuntimeError("short read on payload")
        data += chunk
    return data


def write_frame(sock, payload):
    sock.sendall(struct.pack(">I", len(payload)))
    sock.sendall(payload)


def main():
    url = sys.argv[1] if len(sys.argv) > 1 else "https://example.com"
    request = {
        "method": "GET",
        "url": url,
        "headers": [],
        "body_base64": None,
    }
    payload = json.dumps(request).encode("utf-8")

    response = None
    last_err = None
    for _ in range(10):
        try:
            sock = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
            sock.connect((HOST_CID, PORT))
            write_frame(sock, payload)
            response_bytes = read_frame(sock)
            response = json.loads(response_bytes.decode("utf-8"))
            sock.close()
            break
        except Exception as exc:
            last_err = exc
            try:
                sock.close()
            except Exception:
                pass
            time.sleep(2)

    if response is None:
        raise RuntimeError(f"vsock request failed: {last_err}")

    if response.get("error"):
        error = response["error"]
        code = error.get("code", "unknown_error")
        message = error.get("message", "unknown error")
        print(f"error: {code}: {message}")
        sys.exit(1)

    body_b64 = response.get("body_base64") or ""
    body = base64.b64decode(body_b64.encode("utf-8")) if body_b64 else b""
    print("status=", response.get("status"))
    print(body[:200].decode("utf-8", errors="replace"))


if __name__ == "__main__":
    main()
