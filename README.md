<div align="center" style="align-items: center">

# QualiaMinigames-Core

简体中文 | [English](README.en.md)

QualiaMinigames 的核心库。

承载小游戏集合的全部规则与状态推进逻辑：不含界面与命令行入口，Windows、Android 等
前端各自作为独立项目依赖它。

[![License](https://img.shields.io/github/license/QualiaMinigames/QualiaMinigames-Core?label=License)][License]
[![Releases](https://img.shields.io/github/downloads/QualiaMinigames/QualiaMinigames-Core/total?logo=github&label=Downloads)][Releases]

[License]: LICENSE

[Releases]: https://github.com/QualiaMinigames/QualiaMinigames-Core/releases

</div>

---

## 状态

| 项 | 值 |
|---|---|
| crate 名 | `qualia-minigames-core` |
| 版本 | `0.3.0` |
| 类型 | 纯逻辑库 |
| Edition | 2024 |
| MSRV | **1.89** |
| 开发工具链 | `stable`（由 `rust-toolchain.toml` 钉住） |
| 许可证 | `LGPL-3.0-or-later` |

包含接水管、数独、扫雷、数织与数桥五套完整规则：棋盘生成、回合推进、环路检测、首点保护、
无猜判定、唯一解校验、摆岛与生成树构造、悔棋与存档格式。

## 用法

```toml
[dependencies]
qualia-minigames-core = "0.3"
```

```rust
assert_eq!(
    qualia_minigames_core::VERSION,
    env!("CARGO_PKG_VERSION"),
);
```

## 设计约束

本库是**纯逻辑库**：

- 不访问文件系统、网络与环境变量，不读取系统时钟；
- 时间由调用方以参数传入，单位写在该函数的文档里；
- 随机数由调用方以显式种子传入，因此一局游戏可以仅凭其输入重放；
- 全 crate `#![forbid(unsafe_code)]`。

需要 I/O、平台 API 或 `unsafe` 的部分属于前端或适配层，不属于这个 crate。

## 参与贡献

提交前请确保全绿：

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo doc --no-deps            # 文档链接与 intra-doc link 不得有警告
```

依赖审计与许可证策略（改动依赖后必跑）：

```powershell
cargo deny check
```

`cargo doc` 需带 `RUSTDOCFLAGS="-D warnings"`，否则坏掉的 intra-doc 链接只会被当作警告
打印。提交信息使用英文并遵守
[Conventional Commits](https://www.conventionalcommits.org/)。

### MSRV 策略

`rust-version = "1.89"` 是最低支持声明，不是开发工具链版本：

- **开发工具链**由 `rust-toolchain.toml` 固定为 `stable`，避免各人本机漂移。
- **MSRV 下限**由依赖栈决定，抬高时在对应依赖上注明是谁要求的。
- 改依赖后跑 `cargo +1.89 check --all-targets`，声明的 MSRV 必须真的能编译。

## 许可

本项目按 **GNU Lesser General Public License v3.0 or later**
（`LGPL-3.0-or-later`）授权。你可以依据自由软件基金会发布的 GNU 宽通用公共许可证
第 3 版，或（任选）任何更高版本的条款，再分发和/或修改本作品。

- [`COPYING.LESSER`](COPYING.LESSER)：LGPL-3.0 全文
- [`LICENSE`](LICENSE)：GPL-3.0 全文（LGPL-3.0 通过引用并入）

本作品出于希望其有用的目的分发，但不提供任何担保，甚至不提供适销性或特定用途
适用性的默示担保。详见许可证正文。
