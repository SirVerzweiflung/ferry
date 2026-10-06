# Ferry protocol (v0.2)

v0.2 adds unpaired "guest" sessions, offers, presence discovery and a device type to v0.1.
It stays compatible with v0.1 for paired devices.

TCP port **47800** (data) and UDP port **47800** (discovery). All integers big-endian.
Reference implementations: `desktop/src/proto.rs` and `android/app/src/main/java/dev/ferry/core/Proto.java`
(tested against each other by `tests/interop.py`).

## Handshake (plaintext, 53 bytes each way)

```
client -> server : "FRY1" | mode u8 (1 = session, 2 = pair, 3 = guest) | client_id[16] | client_eph_pub[32]
server -> client : "FRY1" | status u8 | server_id[16] | server_eph_pub[32]
                   status: 0 ok, 1 unknown peer, 2 not pairing, 3 no guests (hidden or blocked), 4 busy (limits)
T = client_hello || server_reply
S = X25519(own_eph_secret, other_eph_pub)       (all-zero result is rejected)
```

**Session** (already paired, long-term key `K`):
`okm = HKDF-SHA256(salt = K, ikm = S, info = "ferry/1 session" || T, 64)`,
`k_c2s = okm[0..32]`, `k_s2c = okm[32..64]`. Authentication is implicit: a party without `K`
cannot produce or read a single frame.

**Pairing** (one side shows a 10-character code from `23456789ABCDEFGHJKMNPQRSTUVWXYZ`,
≈ 49.5 bits, valid 5 minutes, max 3 wrong attempts):
`okm = HKDF-SHA256(salt = code, ikm = S, info = "ferry/1 pair" || T, 128)`;
`conf = okm[0..32]`, `K = okm[32..64]`, `k_c2s = okm[64..96]`, `k_s2c = okm[96..128]`.

```
client -> server : HMAC(conf, "client")      server checks it before answering, so a fake
server -> client : HMAC(conf, "server")      client gets only one online guess per attempt
```

Ephemeral X25519 on every connection gives forward secrecy for sessions.

**Guest** (unpaired, v0.2): `okm = HKDF-SHA256(salt = "ferry/1 guest", ikm = S, info = "ferry/1 guest" || T, 64)`,
split like a session. The connection is encrypted but not authenticated: the receiver treats the
claimed name and id as untrusted and keeps everything in Incoming until the user accepts it.
Before any content the guest sends an OFFER; the receiver ACKs it or refuses it (size limit).

## Frames

`u32 len | ChaCha20-Poly1305(key, nonce = 0x00000000 || u64 counter, aad = "", plaintext)`;
one counter per direction starting at 0. Plaintext = `type u8 | body` (max 1 MiB).
`str` = `u32 length | UTF-8`.

| type | name  | body                                   |
|------|-------|----------------------------------------|
| 1    | HELLO | name str, listen_port u16, flags u32 (low byte: 1 = computer, 2 = phone) — both sides send one first |
| 2    | CLIP  | text str (≤ 512 KiB) → answered by ACK |
| 3    | FILE  | name str, size u64, followed by DATA frames totalling `size` → ACK |
| 4    | DATA  | raw bytes (≤ 64 KiB per frame)         |
| 5    | ACK   | ok u8, message str                     |
| 6    | BYE   | –                                      |
| 7    | OFFER | count u32, total bytes u64, has_text u8 — guests only, before any FILE/CLIP → ACK |

A CLIP from a paired device sets the clipboard. A device that receives clipboard text it didn't
already have passes it on to its other paired devices (never back to the sender). This way all
your devices stay in sync even if they are not all paired with each other. From a guest, CLIP is
a text message that waits in Incoming.

The receiver of a session stores the sender's address as `source_ip:hello.listen_port`.

## Discovery (UDP, port 47800)

**Presence (v0.2):** `magic | id[16] | tcp_port u16 | kind u8 | flags u8 (bit0: accepts unpaired) | name_len u8 | name`

* `"FRYP"` — "who is there?", broadcast to 255.255.255.255 when a device list is opened. It also
  tells the receivers about the sender.
* `"FRYH"` — "here I am": the unicast answer to FRYP. It is also broadcast once at start-up and
  whenever visibility changes. Hidden devices answer only paired devices and send flags = 0.
* Receiving FRYP/FRYH from a device triggers an immediate retry of sends queued for it.

**Paired lookup (v0.1):** `"FRYQ" | requester_id[16] | port u16` → answered (unicast) only if the
requester is a paired device: `"FRYA" | own_id[16] | tcp_port u16`.

## Limits for unpaired devices (receiver side)

Incoming holds at most 10 transfers, at most 2 per sender (by id or IP), and at most
`incoming_limit_mb` (default 2048) in total. A sender IP gets at most 20 sessions per 10 minutes.
Unanswered transfers are deleted after `incoming_hours` (default 24). Blocked devices (id and IP)
are refused with status 3.
