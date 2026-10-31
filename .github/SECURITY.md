# Security Policy

[繁體中文](#安全政策)

## Supported versions

Only the latest minor release line receives security fixes. Fixes ship as a new patch release; upgrade with `./upgrade.sh` ([UPGRADE.md](../docs/UPGRADE.md)).

| Version | Supported |
| --- | --- |
| 0.3.x | Yes |
| < 0.3 | No |

## Reporting a vulnerability

**Please do not open a public issue, pull request or discussion for a vulnerability.**

Report it privately through [GitHub's private vulnerability reporting](https://github.com/hsuanshao/TeamAgentMemory/security/advisories/new). Alternatively, email <william@hsuanshao.com> with "AgentMemory security" in the subject.

Please include:
- the affected version and component: memory-core, Panel, `amem` or a client plugin;
- the steps to reproduce, or a proof of concept;
- what an attacker gains, for example reading another member's memory, or a key leaving the server.

You can expect an acknowledgement within 3 working days, and an assessment with a planned fix within 14 days. We will credit you in the release notes unless you prefer otherwise.

## In scope

Examples of what we treat as vulnerabilities:
- bypassing the user-key authorization of the memory data plane, or the role rules (a member reading another member's L0/L1, a leader opening another member's memory);
- reaching another department's data;
- the Panel proxying a path outside its allowlist, or a browser obtaining the server API key;
- conversation content making the distillation LLM perform actions: prompt injection that leads to tool or command execution;
- secrets written to logs, generation logs or dead letters.

Out of scope: problems that need an already-compromised host or a malicious operator with the server API key, and findings from automated scanners without a demonstrated impact.

---

# 安全政策

## 支援的版本

只有最新的次版本系列會收到安全修正。修正會以新的修補版本發布，請用 `./upgrade.sh` 升級（[UPGRADE.zh-tw.md](../docs/UPGRADE.zh-tw.md)）。

| 版本 | 是否支援 |
| --- | --- |
| 0.3.x | 是 |
| < 0.3 | 否 |

## 回報漏洞

**請不要用公開的 issue、pull request 或 discussion 回報漏洞。**

請透過 [GitHub 的私人漏洞回報](https://github.com/hsuanshao/TeamAgentMemory/security/advisories/new)私下回報，或寄信到 <william@hsuanshao.com>，主旨請包含「AgentMemory security」。

請附上：
- 受影響的版本和元件：memory-core、Panel、`amem` 或 client plugin；
- 重現步驟或概念驗證；
- 攻擊者能取得什麼，例如讀到其他成員的記憶，或 key 被帶出伺服器。

我們會在 3 個工作天內回覆確認收到，並在 14 天內提出評估和修正計畫。除非你不希望公開，否則我們會在 release notes 中致謝。

## 範圍

我們視為漏洞的例子：
- 繞過記憶資料平面的 user key 授權或角色規則，例如成員讀到其他成員的 L0/L1，或 leader 打開其他成員的記憶；
- 存取到其他部門的資料；
- Panel 轉送 allowlist 以外的路徑，或瀏覽器取得伺服器 API key；
- 對話內容讓蒸餾用的 LLM 執行動作，也就是 prompt injection 導致工具或指令被執行；
- 機密資訊被寫進記錄、生成紀錄或 dead letter。

不在範圍內：需要主機已經被入侵，或需要持有伺服器 API key 的惡意維運人員才能利用的問題；以及沒有實際影響證明的自動掃描結果。
