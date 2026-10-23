# HTTPS for clients on other machines

[繁體中文](TLS.zh-tw.md)

Clients send their user key with every request. Over plain HTTP anyone on the network path can read it, so when `amem` or a browser on another machine reaches memory-core or the Panel, put HTTPS in front of them. Docker Compose has an optional profile, `tls`, that runs [Caddy](https://caddyserver.com/) for this: `/v3/*` and `/health` go to memory-core, everything else to the Panel, both on one address.

## 1. Choose the address

| Clients use | Certificate | Needs |
| --- | --- | --- |
| A domain name (`memory.example.com`) | From Let's Encrypt, renewed by Caddy | DNS pointing at this machine, ports 80 and 443 reachable from the internet |
| An IP address or an internal name (`10.0.0.5`, `memory.lan`) | From Caddy's own CA | Each client trusts that CA ([step 3](#3-trust-caddys-ca-ip-address-or-internal-name)) |

## 2. Turn it on

In `.env` (or answer "Other machines over HTTPS" in `install.sh`):

```bash
BIND_ADDR=127.0.0.1                    # memory-core and the Panel: this machine only
TLS_SITE=memory.example.com            # or the IP address clients use
COMPOSE_PROFILES=tls                   # add to what is there, comma-separated (e.g. postgres,tls)
# TLS_HTTPS_PORT=443  TLS_HTTP_PORT=80 # other ports, if those are taken
```

```bash
docker compose up -d
docker compose logs caddy | tail      # "certificate obtained successfully" (domain) or "installing root CA" (IP)
curl -fsS https://memory.example.com/health
```

With `BIND_ADDR=127.0.0.1` the plain-HTTP ports 8420 and 8125 answer only on this machine; other machines go through Caddy. Caddy forwards the client's address, and memory-core trusts it from Caddy and the Panel only (`AGENT_MEMORY_TRUSTED_PROXIES=memory-panel,caddy`), so refused keys are counted per client, not per proxy ([UPGRADE](../UPGRADE.md#refused-keys-are-throttled-and-audited)).

The Panel is served at the same address. Only admins need it; on an address the internet reaches, consider limiting it in `docker/Caddyfile`, e.g. a `remote_ip` matcher for your office or VPN range in front of the last `reverse_proxy`.

## 3. Trust Caddy's CA (IP address or internal name)

Copy the root certificate out once; it stays the same across restarts (it lives in the `caddy-data` volume):

```bash
docker compose cp caddy:/data/caddy/pki/authorities/local/root.crt ./caddy-root.crt
```

- **`amem`**: copy `caddy-root.crt` to each client machine and log in with it: `amem login --server-url https://10.0.0.5 --ca-cert /path/to/caddy-root.crt …` (or `AMEM_CA_CERT`). `amem` trusts its built-in CAs plus this one; the system's trust store is not used.
- **Browsers** (the Panel): import `caddy-root.crt` into the system or browser trust store as a trusted root.

## 4. Point the clients at it

```bash
amem login --server-url https://memory.example.com --department-id default --team-id … --agent-id … --user-key -
```

`amem` does not follow redirects, so a client still configured with the old `http://` address reports the redirect instead of sending its key there; log in again with the `https://` address. The Panel is at `https://memory.example.com/`.

## Another reverse proxy instead

nginx, a load balancer or an ingress works as well: route `/v3/` and `/health` to memory-core (port 8420) and the rest to the Panel (8125), and add the proxy's address or range to `AGENT_MEMORY_TRUSTED_PROXIES` (e.g. `memory-panel,10.0.0.0/8`). memory-core reads `X-Forwarded-For` from the right and stops at the first address it does not trust, so a header a client forges is ignored; never list an address clients can connect from directly.
