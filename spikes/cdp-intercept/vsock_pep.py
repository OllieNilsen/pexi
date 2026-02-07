#!/usr/bin/env python3
"""Vsock PEP client for CDP interception.

Reads a JSON request from stdin, sends it to the PEP stub via vsock,
and writes the JSON response to stdout.

Input format (stdin):
  {"method":"GET","url":"https://example.com","headers":{},"body_base64":null}

Output format (stdout):
  {"status":200,"headers":[["content-type","text/html"]],"body_base64":"..."}
  or
  {"error":{"code":"denied_by_policy","message":"domain not allowlisted"}}
"""
import json
import os
import socket
import struct
import sys


HOST_CID = int(os.getenv("PEP_VSOCK_CID", "2"))
PORT = int(os.getenv("PEP_VSOCK_PORT", "4040"))


def read_frame(sock):
    header = b""
    while len(header) < 4:
        chunk = sock.recv(4 - len(header))
        if not chunk:
            raise RuntimeError("short read on length header")
        header += chunk
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
    request_json = sys.stdin.read()
    request = json.loads(request_json)

    # Normalise headers: convert object to list-of-pairs if needed
    headers = request.get("headers", [])
    if isinstance(headers, dict):
        headers = [[k, v] for k, v in headers.items()]
    request["headers"] = headers

    payload = json.dumps(request).encode("utf-8")

    sock = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
    sock.settimeout(15)
    try:
        sock.connect((HOST_CID, PORT))
        write_frame(sock, payload)
        response_bytes = read_frame(sock)
        response = json.loads(response_bytes.decode("utf-8"))
        json.dump(response, sys.stdout)
    except Exception as exc:
        json.dump({"error": {"code": "vsock_error", "message": str(exc)}}, sys.stdout)
    finally:
        sock.close()


if __name__ == "__main__":
    main()
