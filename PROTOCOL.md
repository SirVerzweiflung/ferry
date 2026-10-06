# Ferry protocol v1

TCP port **47800** (data) and UDP port **47800** (discovery). All integers big-endian.
Reference implementations: `desktop/src/proto.rs` and `android/app/src/main/java/dev/ferry/core/Proto.java`
(tested against each other by `tests/interop.py`).

## Handshake (plaintext, 53 bytes each way)

```
client -> server : "FRY1" | mode u8 (1 = session, 2 = pair) | client_id[16] | client_eph_pub[32]
server -> client : "FRY1" | status u8 (0 ok, 1 unknown peer, 2 not pairing) | server_id[16] | server_eph_pub[32]
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

## Frames

`u32 len | ChaCha20-Poly1305(key, nonce = 0x00000000 || u64 counter, aad = "", plaintext)`;
one counter per direction starting at 0. Plaintext = `type u8 | body` (max 1 MiB).
`str` = `u32 length | UTF-8`.

| type | name  | body                                   |
|------|-------|----------------------------------------|
| 1    | HELLO | name str, listen_port u16, flags u32 (0) — both sides send one first |
| 2    | CLIP  | text str (≤ 512 KiB) → answered by ACK |
| 3    | FILE  | name str, size u64, followed by DATA frames totalling `size` → ACK |
| 4    | DATA  | raw bytes (≤ 64 KiB per frame)         |
| 5    | ACK   | ok u8, message str                     |
| 6    | BYE   | –                                      |

The receiver of a session stores the sender's address as `source_ip:hello.listen_port`.

## Discovery (UDP broadcast to 255.255.255.255:47800)

`"FRYQ" | requester_id[16] | port u16` → answered (unicast) only if the requester is a paired
device: `"FRYA" | own_id[16] | tcp_port u16`. Used only when the stored address no longer works.
