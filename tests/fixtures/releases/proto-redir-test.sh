#!/usr/bin/env bash
# Regression test for --proto-redir in install.sh (issue #1722).
#
# install.sh pins the protocol across redirects with
#
#     curl --proto '=https' --proto-redir '=https' --tlsv1.2 ...
#
# The comment above the call explains why both flags are needed:
# --proto only restricts the initial URL, and a CDN redirect (the
# second hop) is governed by --proto-redir. Without that pin, an
# actor who could answer the first request and redirect the download
# to plain http would hand the installer bytes fetched in cleartext.
#
# Nothing exercised the flag. This fixture serves a 302 from an https
# origin to an http:// URL and requires the installer to refuse at
# the download step. The refusal has to be attributed: an installer
# that fails for an unrelated reason would still exit non-zero, and a
# case that only checked the exit code would pass with --proto-redir
# deleted - the signature check would catch a bad payload and produce
# a non-zero exit anyway.
#
# The redirect target serves a VALID payload - the real fixture
# artifact. If the flag were removed, curl would follow the redirect
# and the download would succeed; the case would then fail because the
# install did NOT refuse at the download step (and did install, if it
# got that far). That is the trap the valid payload closes.
#
# Run from the repo root:
#   bash tests/fixtures/releases/proto-redir-test.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
INSTALL_SH="$REPO_ROOT/install.sh"
FIXTURE_DIR="$REPO_ROOT/tests/fixtures/releases"

# Pre-flight: the test spins up local HTTP/HTTPS listeners and needs
# python3 (stdlib http.server + ssl) and openssl (self-signed cert).
# A missing tool here is a missing fixture, not a passing test.
command -v python3 >/dev/null 2>&1 || {
	echo "SKIP: python3 is not installed"
	exit 0
}
command -v openssl >/dev/null 2>&1 || {
	echo "SKIP: openssl is not installed"
	exit 0
}

# Source install.sh's helpers (everything before the "# --- Dispatch"
# section). Same pattern version-self-test.sh uses: the helpers are
# pure function definitions and constants; sourcing them does not
# touch the network or write to the filesystem.
TMP_HELPERS="$(mktemp)"
sed '/^# --- Dispatch /,$d' "$INSTALL_SH" > "$TMP_HELPERS"
# shellcheck disable=SC1090
source "$TMP_HELPERS"

# Work directory: self-signed cert, server logs, captured output. The
# install.sh source installed a trap of its own (rm -rf "$TMP_DIR")
# on EXIT; we override both TMP_DIR and the trap so cleanup runs in
# one place and the listener's life cycle is bounded.
WORK="$(mktemp -d)"
SRV_PID=""
cleanup() {
	if [[ -n "$SRV_PID" ]]; then
		kill "$SRV_PID" 2>/dev/null || true
		wait "$SRV_PID" 2>/dev/null || true
	fi
	rm -f "$TMP_HELPERS"
	rm -rf "$WORK"
}
trap cleanup EXIT
TMP_DIR="$WORK/dl"
mkdir -p "$TMP_DIR"

# Free ports on 127.0.0.1 for the HTTPS (302) and HTTP (payload)
# listeners. Picking at runtime avoids colliding with anything the
# runner already has open.
HTTPS_PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
HTTP_PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"

# Self-signed cert with SAN=127.0.0.1 so curl accepts it once
# CURL_CA_BUNDLE points at it. Without that env var curl would refuse
# the cert before reaching the redirect, and the case would fail at
# the TLS handshake rather than at the redirect - the protection
# under test would be invisible.
openssl req -x509 -newkey rsa:2048 \
	-keyout "$WORK/cert.key" -out "$WORK/cert.pem" -days 1 -nodes \
	-subj "/CN=localhost" \
	-addext "subjectAltName = IP:127.0.0.1" >/dev/null 2>&1

# HTTPS origin (the URL install.sh will hit) and HTTP target (where
# the 302 redirects to). Both listeners bind 127.0.0.1; nothing here
# is reachable from off-host.
HTTPS_URL="https://127.0.0.1:$HTTPS_PORT/asset"
HTTP_TARGET="http://127.0.0.1:$HTTP_PORT/asset"

# One Python process, two listeners: HTTPS returns 302 to the HTTP
# port, HTTP serves the fixture artifact bytes. Daemon threads do the
# serving; the main thread blocks on a signal so SIGTERM stops the
# process promptly instead of waiting out a sleep.
# `env` rather than a bare assignment prefix: with the prefix form, a later
# assignment on the same line expands the OUTER value, which shellcheck flags
# (SC2097/SC2098) and which is a real trap the day these stop being equal.
env WORK="$WORK" HTTPS_PORT="$HTTPS_PORT" HTTP_PORT="$HTTP_PORT" \
	HTTP_TARGET="$HTTP_TARGET" FIXTURE_DIR="$FIXTURE_DIR" \
	python3 - >"$WORK/srv.log" 2>&1 <<'PYEOF' &
import http.server, ssl, socketserver, threading, signal, os

WORK = os.environ['WORK']
HTTPS_PORT = int(os.environ['HTTPS_PORT'])
HTTP_PORT = int(os.environ['HTTP_PORT'])
HTTP_TARGET = os.environ['HTTP_TARGET']
FIXTURE_DIR = os.environ['FIXTURE_DIR']


class RedirHandler(http.server.BaseHTTPRequestHandler):
	def do_GET(self):
		try:
			self.send_response(302)
			self.send_header('Location', HTTP_TARGET)
			self.end_headers()
		except (BrokenPipeError, ConnectionResetError):
			pass

	def log_message(self, *a, **k):
		pass


class AssetHandler(http.server.BaseHTTPRequestHandler):
	def do_GET(self):
		try:
			# Serve the real fixture artifact: a download that
			# arrives here is a valid binary by definition. If
			# --proto-redir were removed, curl would follow the
			# 302 and get bytes - and the install would NOT
			# refuse at the download step.
			with open(os.path.join(FIXTURE_DIR, 'podup-linux-x86_64'), 'rb') as f:
				data = f.read()
			self.send_response(200)
			self.send_header('Content-Type', 'application/octet-stream')
			self.send_header('Content-Length', str(len(data)))
			self.end_headers()
			self.wfile.write(data)
		except (BrokenPipeError, ConnectionResetError):
			pass

	def log_message(self, *a, **k):
		pass


ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(f'{WORK}/cert.pem', f'{WORK}/cert.key')

httpd = socketserver.TCPServer(('127.0.0.1', HTTPS_PORT), RedirHandler)
httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
threading.Thread(target=httpd.serve_forever, daemon=True).start()

httpd2 = socketserver.TCPServer(('127.0.0.1', HTTP_PORT), AssetHandler)
threading.Thread(target=httpd2.serve_forever, daemon=True).start()

stop = threading.Event()
signal.signal(signal.SIGTERM, lambda *_: stop.set())
signal.signal(signal.SIGINT, lambda *_: stop.set())
with open(f'{WORK}/srv.ready', 'w') as f:
	f.write('ok')
stop.wait()
PYEOF
SRV_PID=$!

# Poll for readiness rather than sleeping a guessed interval, so the
# suite is deterministic on a slow runner. A server that never came
# up is a missing fixture, not a passing test.
for _ in $(seq 1 50); do
	[[ -f "$WORK/srv.ready" ]] && break
	sleep 0.1
done
if [[ ! -f "$WORK/srv.ready" ]]; then
	echo "FAIL: the local redirect server never came up"
	echo "      server log:"
	sed 's/^/        /' "$WORK/srv.log"
	exit 1
fi

# Point BASE_URL at the HTTPS origin so install.sh's download
# function hits the 302 listener. CURL_CA_BUNDLE trusts the cert.
BASE_URL="$HTTPS_URL"
export CURL_CA_BUNDLE="$WORK/cert.pem"

# Call install.sh's download function with the redirect URL. The
# function passes --proto-redir '=https' to curl, which refuses the
# 302 to http:// and makes the function fall through to
# `fail "Download failed: ..."`. We run inside a subshell so the
# function's `exit 1` only kills the subshell; the test then reads
# the exit status and the captured output.
set +e
(download "${BASE_URL}/asset" "${TMP_DIR}/asset") >"$WORK/out" 2>&1
rc=$?
set -e

# Three assertions, all of them needed:
#
#   1. non-zero exit: the install refused.
#   2. "Download failed" in stderr: the refusal is at the download
#      step, not at some later check. Without this attribution, an
#      installer that refuses for any other reason would satisfy the
#      exit-code check alone.
#   3. nothing written to TMP_DIR: the refused download produced no
#      artifact, so a half-written binary cannot be left on disk.
#
# All three must hold. Removing --proto-redir flips all three at once:
# curl follows the 302 to the HTTP listener, the download succeeds,
# and the install would proceed past the download step.
fail=0
if [[ "$rc" -eq 0 ]]; then
	echo "FAIL: download should have refused the redirect, but succeeded"
	echo "      output:"
	sed 's/^/        /' "$WORK/out"
	fail=$((fail + 1))
fi
if ! grep -q "Download failed" "$WORK/out"; then
	echo "FAIL: download should have failed with 'Download failed', got:"
	sed 's/^/        /' "$WORK/out"
	fail=$((fail + 1))
fi
if [[ -e "${TMP_DIR}/asset" ]]; then
	echo "FAIL: nothing should have been written on download refusal,"
	echo "      but ${TMP_DIR}/asset exists ($(stat -c '%s' "${TMP_DIR}/asset") bytes)"
	fail=$((fail + 1))
fi

if [[ "$fail" -ne 0 ]]; then
	exit 1
fi

echo "All parts passed."
