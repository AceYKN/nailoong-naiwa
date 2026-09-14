# NLNF Reference Matcher

奶龙 / 奶蛙的 Windows 本地参考图特征匹配与 QQ 安全处理系统。

当前产品规格是 [Specification v2.0](spec-v2.md)：零训练、零云端预标注、零 ONNX。系统将每类 1~10 张参考图作为 Reference Bank，使用 pHash 粗筛、SIFT/AKAZE 局部特征、Lowe Ratio Test 和 RANSAC 几何验证，输出 `NAILONG`、`NAIWA_FROG`、`OTHER` 或 `UNKNOWN`。

## 当前状态

- v2 规格已经固化到 `spec-v2.md`。
- Rust 决策层、pHash、ReferenceManager、参考图 SQLite 表和新版桌面 UI 已实现；当前 Rust 单元测试与前端构建可独立运行。
- OpenCV SIFT、RANSAC 和版本化描述子缓存已接入 feature-gated 后端；未设置原生依赖时桌面识别按钮仍会 fail closed，不会伪造 v2 识别结果。
- OneBot 11 loopback Adapter 已接入桌面 QQ 页面：连接、反向事件、OFF/OBSERVE/AUTO_RECALL 群模式和 moderation_log 持久化均有本机 mock E2E；默认仍为 OFF。
- QQ 默认保持 `OFF`。真实 Adapter、Observe 和 Auto Recall 都必须在本地测试与人工验收后才会开放。
- 公开仓库已创建为 [`AceYKN/nailoong-naiwa`](https://github.com/AceYKN/nailoong-naiwa)；公开内容只包含干净源码和文档，不包含 QQ 缓存、验证图片、模型、安装包或密钥。
- v1 的数据集、训练、ONNX、模型包、DeepSeek 预标注和批量标注工具已从当前 checkout 移除；不会删除用户 QQ 缓存图片。

## 目录

```text
apps/desktop/                 Tauri 2 + React + TypeScript 桌面端
apps/desktop/src-tauri/src/   Rust 输入边界、视觉决策、ReferenceManager、QQ Adapter
references/                   本地参考图库（每类 1~10 张）
tests/                        传统视觉、负例、变换、GIF 和 QQ Mock 测试
docs/                         架构、路线图和验收证据
spec-v2.md                    当前权威规格
tools/feature_match/          Windows OpenCV/Clang 环境与打包辅助脚本
tools/validation/              冻结验证 manifest 生成与发布门禁脚本
```

## 开发命令

```powershell
pnpm install
pnpm typecheck
pnpm build
pnpm --dir apps/desktop tauri:dev
# 在已设置 OpenCV/Clang 环境变量后运行真实 SIFT 后端
pnpm --dir apps/desktop tauri:dev:opencv
# 在已设置 OpenCV/Clang 环境变量后生成带 runtime DLL 的 NSIS 包
pnpm --dir apps/desktop tauri:build:opencv
```

Rust 核心测试：

```powershell
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml
cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path apps/desktop/src-tauri/Cargo.toml --all-targets -- -D warnings
```

若要执行正式冻结验证，先准备独立于仓库的 truth-reviewed 验证材料，再
按 [`tools/validation/README.md`](tools/validation/README.md) 生成 manifest
并运行 release gate。该流程只验证，不训练、不上传图片。

OpenCV 是生产视觉后端且默认 feature-gated。Windows 原生依赖、环境变量和验证命令见 [`tools/feature_match/README.md`](tools/feature_match/README.md)；未配置它时仍可运行跨平台的 fail-closed 核心和 UI 检查。

## 运行原则

1. 一类至少 1 张、最多 10 张参考图；新增参考图立即递增 `reference_set_version`。
2. 识别失败或证据不足返回 `UNKNOWN`，不生成猜测概率。
3. QQ `AUTO_RECALL` 必须同时满足高分、严格 margin、最小 inliers、inlier ratio、coverage、重投影误差和 `VERY_HIGH` confidence；未通过冻结验证门禁的普通构建不会开放该模式。
4. 图片默认只在本机处理；v2 不调用云端预标注服务，也不会上传新图片。

完整阶段、迁移边界和未完成验收项见 [docs/ROADMAP.md](docs/ROADMAP.md)、[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) 和 [docs/ACCEPTANCE_MATRIX.md](docs/ACCEPTANCE_MATRIX.md)。
