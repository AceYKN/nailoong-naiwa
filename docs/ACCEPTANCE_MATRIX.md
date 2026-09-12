# 验收证据矩阵

截至 2026-09-13，本文件只记录当前 checkout 中可复核的 v2 证据。代码存在、路线图和浏览器预览不等于功能已验收。

## 已验证

| 范围 | 证据 |
| --- | --- |
| v2 方向 | `spec-v2.md` 已将产品定义为零训练、多参考图、传统特征匹配；DeepSeek、训练、ONNX 不属于运行依赖。 |
| Rust 决策层 | `cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --lib`：59 项通过；覆盖 pHash、参考图数量、分数、普通分类几何证据门槛、`OTHER/UNKNOWN`、置信度、动画严格门槛、存储、QQ 服务状态和严格门槛。 |
| Rust 质量门 | 默认 feature 的 fmt/clippy 通过；OpenCV feature 的 clippy 也通过。 |
| ReferenceManager | 本地单元测试验证 1~10 张上限、源文件不修改、SHA-256/pHash 元数据与原子复制。 |
| SQLite v2 | `settings`、`reference_images`、`prediction_cache` 与 `reference_set_version` 已实现；重开幂等、版本化缓存和参考库增删测试通过。 |
| 前端 | `pnpm typecheck` 与 `pnpm build` 通过；四页已切换到识别、QQ、参考图库、设置。 |
| 参考图初始化向导 | 隔离 Tauri/WebView 首次运行显示奶龙/奶蛙两步向导；从本地 QQ 缓存各选择 1 张后分别写入 Reference Bank、刷新版本并显示完成状态，识别页向导消失。 |
| 桌面窗口 E2E | 使用隔离标识符 `com.aceykn.nlnfclassifier.e2e` 启动真实 Tauri/WebView 窗口；从 D 盘 QQ 缓存添加奶龙、奶蛙各 1 张，参考库版本 1→3、各显示 1/10；上传两张待识别图并点击两次“开始匹配”均返回结果卡片，调试按钮生成 `SIFT 与 RANSAC 匹配特征点` PNG。跨盘保存错误已修复，源文件 SHA-256 与 manifest 保持一致。 |
| QQ OneBot 桌面 E2E | 使用本机 mock OneBot API 返回群组，真实 Tauri 窗口连接 API 与反向事件监听端口；群组可切换到 OBSERVE，发送带图片的反向事件后收到 HTTP 200 ACK，后台通过 `get_image` 下载并完成本地分类，UI 显示事件，SQLite `moderation_log` 持久化并可回读。该测试未启用撤回。 |
| QQ Mock 安全路径 | `qq.rs` 单测覆盖重复消息 at-most-once、Adapter 断线不撤回、图片获取/分类失败 fail-closed，以及撤回失败记录且不重试。 |
| 当前图片加入参考库 | 识别队列可明确选择奶龙或奶蛙，将图片复制进对应 Reference Bank 并立即刷新版本；不会修改源文件。 |
| 输入边界 | Rust 侧保留 25 MiB、8192×8192、5000 万像素、500 帧，以及 PNG/JPEG/GIF/WebP 的受限检查。 |
| v1 链路清理 | 数据集、训练、ONNX、模型包、DeepSeek 预标注和批量标注工具已从当前 checkout 移除；用户 QQ 缓存图片未被删除。 |

## 已实现但尚未正式验收

| 范围 | 当前状态与缺失证据 |
| --- | --- |
| OpenCV SIFT | `vision_opencv.rs` 已实现 pHash 一级短路、BFMatcher KNN、Lowe ratio、RANSAC Homography、coverage、reprojection error、调试匹配图、版本化颜色/描述子缓存和明显色差负向辅助惩罚；用户态 OpenCV 4.13.0 + Clang 环境下 `cargo check`、feature clippy 和 71 项 feature 单测通过。pHash 短路没有几何证据，因此不满足 QQ 撤回门槛。 |
| OpenCV 冒烟 | feature 单测验证自生成纹理图的 SIFT 几何自匹配、缩放/旋转/裁剪/JPEG/文字/模糊鲁棒性、无关纹理拒绝、描述子缓存 round-trip、PNG 调试图和静态 P95；2026-09-13 显式环境变量指向本地缓存图片时，Rust Windows decoder + OpenCV smoke 实测奶龙 `NAILONG 0.980`、奶蛙 `NAIWA_FROG 0.980`，OTHER 图片返回 `OTHER`，真实静态 P95 `1.4717ms`。本地 304 条验证 manifest（奶龙 178、奶蛙 4、OTHER 122）重新处理 304、跳过 0、奶龙正确 1、奶蛙正确 2、OTHER/UNKNOWN 122、`false_target_label=0`、`false_recall=0`；真实动画为 18 帧抽样 12 帧，P95 `114.9185ms`，未达到奶蛙严格门槛。该证据不是完整准确率验收，也不是训练数据或发布包内容。 |
| Windows NSIS 本机包 | 2026-09-13 OpenCV 专用 Tauri release build 与 NSIS 产物退出码 0；包含初始化向导、重复参考图提示和 QQ worker 生命周期修复的最新安装包 18,407,964 bytes，SHA-256 `B31B41590721EE83AE3D0C2340A524E70ECBFEF4CDD2E17D9346CCE9EE8615E5`。隔离目录安装后包含 `nlnf-desktop.exe`、`opencv_world4130.dll` 和 `uninstall.exe`；静默安装退出码 0、卸载退出码 0，卸载后目录消失。此前同流程还验证过当前机器启动窗口和开始菜单快捷方式；当前包未启动正式标识符实例，以避免修改既有正式应用数据。仅代表当前机器，未覆盖第二台机器、Defender 或签名。 |
| 动图 | Rust decoder 已有采样边界，视觉层有多帧严格奶蛙门槛；真实 GIF smoke 已验证解码和性能，但仍缺真实奶蛙动画正例、Animated WebP 和完整 QQ 场景证据。 |
| 负例与鲁棒性 | 已有缩放鲁棒性、无关纹理拒绝、RANSAC 误匹配、低 inlier 高分拒绝、色彩辅助惩罚单测；同一份本地 304 条 manifest 重新验证后 `false_target_label=0`、`false_recall=0`，但仍缺冻结的裁剪、压缩、遮挡、文字覆盖、黄色卡通等测试集和更广泛正例覆盖。 |
| QQ | OneBot 11 已接入桌面后台 worker：仅允许 loopback、Token 只在内存、反向事件有界读取、群模式持久化、Observe 日志持久化/回读；本机 mock OneBot E2E 已通过。真实 QQ/OneBot 账号和人工 Observe 运行仍未验证，因此 AUTO_RECALL 仍不作为发布能力开放。 |

## 明确未完成

| 范围 | 状态 |
| --- | --- |
| OpenCV 原生环境 | 已在用户态工具目录完成一次 OpenCV 4.13.0 + Clang 验证；开发环境和 OpenCV 专用 NSIS staging 已可复现，第二台机器仍需独立验证。 |
| `OTHER` / `UNKNOWN` | 已区分：低分且两类均无几何 inlier 返回 `OTHER`；模糊或有部分证据但不足以确定时返回 `UNKNOWN`。仍需真实负例冻结集校准边界。 |
| 公开 GitHub | 仍未创建或推送公开仓库。旧历史含本地机器路径，清洁首提交和公开推送需要用户在执行前确认。 |
| DeepSeek key | 不再调用或上传图片；此前暴露过旧 key，用户应在服务商侧撤销。新 key 只要仍在本机用户环境变量中，也建议完成迁移后清除。 |

## 重跑核心验证

```powershell
cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml -- --check
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --lib
cargo clippy --manifest-path apps/desktop/src-tauri/Cargo.toml --all-targets -- -D warnings
pnpm --dir apps/desktop typecheck
pnpm --dir apps/desktop build
```

OpenCV feature build（需先按 `tools/feature_match/README.md` 设置用户态依赖变量）：

```powershell
cargo check --manifest-path apps/desktop/src-tauri/Cargo.toml --features opencv-backend
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --features opencv-backend --lib -- --test-threads=1
```
