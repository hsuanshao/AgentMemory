# Licensing and commercial use

[繁體中文](COMMERCIAL.zh-tw.md)

From version 0.4.0, the AgentMemory server is licensed under the [Business Source License 1.1](LICENSE) (BSL). The source stays public: you can read it, change it, build it and contribute to it. Production use beyond the free tiers below needs a commercial license. This page explains the [LICENSE](LICENSE) in plain words; where they differ, the LICENSE is what counts.

## What is free

| Use | Free? |
| --- | --- |
| Evaluation, development and testing, in any organization | Yes, always |
| An individual, for personal and non-commercial purposes | Yes |
| A non-profit, or an accredited school or research institution, for its own non-commercial purposes | Yes |
| Any other organization, with at most **1 department and 10 members** in total across all its deployments | Yes |
| Any other organization beyond that | Needs a commercial license |
| Offering AgentMemory, or a product built mainly on it, to others as a hosted, managed or embedded service | Needs a commercial license |

- An **organization** includes every company that controls it, that it controls, or that is under common control with it.
- A **department** is one isolated data space on a memory-core server ([COLLABORATION.md](docs/COLLABORATION.md)).
- A **member** is a user account that can read or write memories. The agents a member runs are not counted separately.

## What stays MIT

- **Released versions:** 0.3.2 and earlier remain under the MIT License, permanently.
- **Clients:** the `amem` client (`crates/memory-client/`) and the agent plugins (`clients/`) stay under the MIT License, so any tool can integrate with an AgentMemory server.

## When each version becomes open source

Each version converts to the **Apache License 2.0** four years after it is published. For example, a version released on 2027-01-15 becomes Apache 2.0 on 2031-01-15.

## Buying a commercial license

Write to **william@hsuanshao.com**. Include:
- your organization;
- roughly how many departments and members you need;
- whether you run it yourself or offer it to others.
