# 讓其他機器上的 client 走 HTTPS

[English](TLS.md)

Client 每個請求都會帶著 user key。走一般 HTTP 時，網路路徑上的任何人都讀得到，所以當其他機器上的 `amem` 或瀏覽器要連 memory-core 或 Panel 時，請在前面加上 HTTPS。Docker Compose 有一個可選的 profile `tls`，會為此啟動 [Caddy](https://caddyserver.com/)：`/v3/*` 和 `/health` 轉給 memory-core，其餘轉給 Panel，兩者共用同一個位址。

## 1. 決定位址

| Client 使用 | 憑證 | 需要 |
| --- | --- | --- |
| 網域名稱（`memory.example.com`） | 由 Let's Encrypt 簽發，Caddy 自動更新 | DNS 指向這台機器，且網際網路連得到 80 與 443 port |
| IP 位址或內部名稱（`10.0.0.5`、`memory.lan`） | 由 Caddy 自己的 CA 簽發 | 每個 client 都要信任這個 CA（[步驟 3](#3-信任-caddy-的-caip-位址或內部名稱)） |

## 2. 啟用

在 `.env`（或在 `install.sh` 選「Other machines over HTTPS」）：

```bash
BIND_ADDR=127.0.0.1                    # memory-core 與 Panel：只有這台機器
TLS_SITE=memory.example.com            # 或 client 使用的 IP 位址
COMPOSE_PROFILES=tls                   # 加進現有的值，以逗號分隔（例如 postgres,tls）
# TLS_HTTPS_PORT=443  TLS_HTTP_PORT=80 # 這兩個 port 被占用時改用其他 port
```

```bash
docker compose up -d
docker compose logs caddy | tail      # 「certificate obtained successfully」（網域）或「installing root CA」（IP）
curl -fsS https://memory.example.com/health
```

設 `BIND_ADDR=127.0.0.1` 後，一般 HTTP 的 8420 與 8125 只在這台機器上回應；其他機器透過 Caddy 連線。Caddy 會轉送 client 的位址，而 memory-core 只信任 Caddy 與 Panel 轉送的位址（`AGENT_MEMORY_TRUSTED_PROXIES=memory-panel,caddy`），所以錯誤的 key 會依 client 計算，而不是依代理（[UPGRADE](../UPGRADE.zh-tw.md#錯誤的-key-會被節流並記錄)）。

Panel 也在同一個位址。只有管理者需要它；如果這個位址對網際網路開放，可以考慮在 `docker/Caddyfile` 限制它，例如在最後一個 `reverse_proxy` 前加上辦公室或 VPN 網段的 `remote_ip` matcher。

## 3. 信任 Caddy 的 CA（IP 位址或內部名稱）

把根憑證複製出來一次即可；重新啟動也不會變（它存在 `caddy-data` volume）：

```bash
docker compose exec -T caddy cat /data/caddy/pki/authorities/local/root.crt > caddy-root.crt
```

- **`amem`**：把 `caddy-root.crt` 複製到每台 client 機器，登入時指定它：`amem login --server-url https://10.0.0.5 --ca-cert /path/to/caddy-root.crt …`（或設 `AMEM_CA_CERT`）。`amem` 信任內建的 CA 加上這一個；不使用系統的信任庫。
- **瀏覽器**（Panel）：把 `caddy-root.crt` 匯入系統或瀏覽器的信任庫，設為受信任的根憑證。

## 4. 把 client 指過去

```bash
amem login --server-url https://memory.example.com --department-id default --team-id … --agent-id … --user-key -
```

`amem` 不跟隨轉址，所以還設定成舊 `http://` 位址的 client 會回報轉址，而不會把 key 送過去；用 `https://` 位址重新登入即可。Panel 在 `https://memory.example.com/`。

## 改用其他反向代理

nginx、負載平衡器或 ingress 也可以：把 `/v3/` 和 `/health` 轉給 memory-core（8420 port），其餘轉給 Panel（8125），並把代理的位址或網段加進 `AGENT_MEMORY_TRUSTED_PROXIES`（例如 `memory-panel,10.0.0.0/8`）。memory-core 從右邊讀 `X-Forwarded-For`，遇到第一個不信任的位址就停下，所以 client 偽造的 header 會被忽略；絕對不要列入 client 能直接連過來的位址。
