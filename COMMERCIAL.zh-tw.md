# 授權與商業使用

[English](COMMERCIAL.md)

從 0.4.0 版開始，AgentMemory 的 server 端改用 [Business Source License 1.1](LICENSE)（BSL）授權。原始碼仍然公開，你可以閱讀、修改、自行建置，也可以參與貢獻。超出下面免費範圍的正式環境使用，需要購買商業授權。本頁用白話說明 [LICENSE](LICENSE)；兩者不一致時，以 LICENSE 為準。

## 哪些使用是免費的

| 使用情況 | 是否免費 |
| --- | --- |
| 任何組織的評估、開發和測試 | 是，一律免費 |
| 個人的非商業使用 | 是 |
| 非營利組織，或立案的學校、研究機構，用於自身的非商業目的 | 是 |
| 其他組織，所有部署合計最多 **1 個部門、10 個成員** | 是 |
| 其他組織超過上述規模 | 需要商業授權 |
| 把 AgentMemory，或主要建立在它之上的產品，做成託管、代管或嵌入式服務提供給他人 | 需要商業授權 |

- **組織**：包含控制它、受它控制，或與它受同一方控制的所有公司。
- **部門**：memory-core 上一個獨立的資料空間（[COLLABORATION.zh-tw.md](docs/COLLABORATION.zh-tw.md)）。
- **成員**：可以讀寫記憶的使用者帳號。成員使用的 agent 不另外計算。

## 哪些維持 MIT

- **已發布的版本**：0.3.2 以前的版本，永遠維持 MIT 授權。
- **Client**：`amem` client（`crates/memory-client/`）和各工具的 plugin（`clients/`）維持 MIT 授權，任何工具都能自由串接 AgentMemory server。

## 各版本何時轉為開源

每個版本在發布滿四年後，會轉為 **Apache License 2.0**。例如 2027-01-15 發布的版本，在 2031-01-15 轉為 Apache 2.0。

## 購買商業授權

請來信 **william@hsuanshao.com**，並附上：
- 貴組織的名稱；
- 大約需要幾個部門、幾位成員；
- 是自行部署，還是提供給他人使用。
