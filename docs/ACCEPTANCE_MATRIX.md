# 验收证据矩阵

截至 2026-09-15，本文件只记录当前 checkout 中可复核的 v2 证据。代码存在、路线图和浏览器预览不等于功能已验收。

## 已验证

| 范围 | 证据 |
| --- | --- |
| v2 方向 | `spec-v2.md` 已将产品定义为零训练、多参考图、传统特征匹配；DeepSeek、训练、ONNX 不属于运行依赖。 |
| Rust 决策层 | `cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --all-targets`：本次本机 77 项库测试通过；覆盖 pHash、参考图数量、分数、普通分类几何证据门槛、`OTHER/UNKNOWN`、置信度、动画严格门槛、WebP 静态/动画回退、存储、QQ 服务状态和严格门槛。新增回归测试确认普通构建拒绝 `AUTO_RECALL`，旧数据库中的该模式降级为 `OBSERVE`，参考图增删或识别阈值变更会在同一 SQLite 事务内持久降级为 `OBSERVE`，并且多图消息任一图片下载/识别失败时自动撤回降级为 Observe；release manifest 也拒绝重复路径。连接状态还会按每个群组重新读取持久化模式、阈值和能力，数据库异常或记录缺失时界面 fail closed。 |
| Rust 质量门 | 默认 feature 的 fmt/clippy 通过；本机 OpenCV feature `cargo test` 通过 92 项库测试，feature clippy 也通过；提交 `e3a055af9c9104608f28a5d0e3af9322c2c9f616` 的 Windows OpenCV feature CI run `34965613779` 六项 job 全部通过 check/test/clippy、Tauri OpenCV NSIS 构建、runtime staging、合成 release plumbing、资源检查和 artifact 上传。release feature 的结构合法测试凭证绑定规则仍保留，缺少凭证或证书 SHA 与当前 checkout 不一致时会拒绝编译。证书现同时绑定完整 Reference Bank、manifest、视觉/缓存指纹和 OpenCV runtime SHA-256。 |
| ReferenceManager | 本地单元测试验证 1~10 张上限、源文件不修改、SHA-256/pHash 元数据与原子复制。 |
| 内置默认参考库 | `builtin_references` 单元测试确认 1 张奶龙与 6 张奶蛙资源均被编译进程序；Windows 启动初始化会复制到 app-data Reference Bank，并通过持久化标记避免重复导入，已有非内置用户参考图的类别保持不变。新增“补充内置参考图”命令会跳过已有 SHA、遵守每类 10 张上限并保持幂等。 |
| SQLite v5 | `settings`、`reference_images`、`prediction_cache`、`moderation_messages`、`classification_label`、`reference_set_version` 与 `engine_fingerprint` 已实现；重开幂等、完整分类缓存、跨引擎隔离、版本化缓存、按群幂等和参考库增删测试通过。参考图增删或识别阈值变更会在事务内持久将现有 `AUTO_RECALL` 群组降级为 `OBSERVE`；旧 v4 缓存迁移为无指纹条目并自动失效。 |
| 前端 | `pnpm typecheck` 与 `pnpm build` 通过；四页已切换到识别、QQ、参考图库、设置；识别页支持单张/多选文件和本地缓存文件夹选择，仍按 120 张批次上限读取且不移动源文件；参考图库支持一次多选，按剩余名额顺序逐张写入并汇总失败，也支持为已有安装补充缺失的内置参考图；设置页可持久化阈值、开发者模式和 SQLite 诊断。浏览器预览检查无 console error。 |
| 参考图初始化向导 | 隔离 Tauri/WebView 首次运行显示奶龙/奶蛙两步向导；从本地 QQ 缓存各选择 1 张后分别写入 Reference Bank、刷新版本并显示完成状态，识别页向导消失。 |
| 桌面窗口 E2E | 使用隔离标识符 `com.aceykn.nlnfclassifier.e2e` 启动真实 Tauri/WebView 窗口；从 D 盘 QQ 缓存添加奶龙、奶蛙各 1 张，参考库版本递增、各显示 1/10；上传奶龙、奶蛙和负例各 1 张并点击“开始匹配”，结果分别为奶龙 98%、奶蛙 98%、其他。新增回归在旧的 1+1 参考库上点击“补充内置参考图”后变为奶龙 2/10、奶蛙 7/10，用户参考图保持不变；再次点击无新增且版本不变。动画参考图 pHash 采样策略已统一，调试按钮仍可生成 `SIFT/AKAZE 与 RANSAC 匹配特征点` PNG。跨盘保存错误已修复，源文件 SHA-256 与 manifest 保持一致；本次真实 Tauri 检查无 console error。 |
| QQ OneBot 桌面 E2E | 使用本机 mock OneBot API 返回群组，真实 Tauri 窗口连接 API 与反向事件监听端口；群组可切换到 OBSERVE，发送带图片的反向事件后收到 HTTP 200 ACK，后台通过 `get_image` 下载并完成本地分类，UI 显示事件，SQLite `moderation_log` 持久化并可回读。该测试未启用撤回。 |
| QQ Mock 安全路径 | `qq.rs` 单测覆盖重复消息 at-most-once、Adapter 断线不撤回、图片获取/分类失败 fail-closed，以及撤回失败记录且不重试。 |
| 当前图片加入参考库 | 识别队列可明确选择奶龙或奶蛙，将图片复制进对应 Reference Bank 并立即刷新版本；不会修改源文件。 |
| 输入边界 | Rust 侧保留 25 MiB、8192×8192、5000 万像素、500 帧，以及 PNG/JPEG/GIF/WebP 的受限检查。 |
| v1 链路清理 | 数据集、训练、ONNX、模型包、DeepSeek 预标注和批量标注工具已从当前 checkout 移除；用户 QQ 缓存图片未被删除。 |
| 公开仓库 | 用户已确认公开创建 `AceYKN/nailoong-naiwa`；`main` 与 `origin/main` 已同步到提交 `e3a055af9c9104608f28a5d0e3af9322c2c9f616`，对应 CI run `34965613779` 六项 job 全部通过。 |
| Windows 验证 artifact | CI run `34965613779` 的 `rust-windows-opencv` 上传步骤成功；artifact 名为 `nlnf-windows-opencv-e3a055af9c9104608f28a5d0e3af9322c2c9f616`，大小 58,196,075 bytes，当前未过期，保留至 2026-09-29。 |

## 已实现但尚未正式验收

| 范围 | 当前状态与缺失证据 |
| --- | --- |
| OpenCV SIFT | `vision_opencv.rs` 已实现 pHash 一级短路、SIFT 主路径、SIFT 无可用描述子时的 AKAZE fallback、按描述子类型选择 BFMatcher 距离、Lowe ratio、RANSAC Homography、coverage、reprojection error、调试匹配图、版本化颜色/描述子缓存和明显色差负向辅助惩罚；描述子缓存现绑定实际源图 SHA-256、提取器种类与提取器指纹，预测缓存绑定源码/配置/采样引擎指纹。本机 OpenCV feature 92 项库测试通过；公开 CI 以固定 OpenCV 4.13.0 + LLVM 20.1.8 完成 Windows 原生 check/test/clippy、Tauri OpenCV NSIS、runtime staging、release plumbing、资源检查和 artifact 上传。pHash 短路没有几何证据，因此不满足 QQ 撤回门槛。 |
| OpenCV 冒烟 | feature 单测验证自生成纹理图的 SIFT 几何自匹配、缩放/旋转/裁剪/JPEG/文字/模糊鲁棒性、无关纹理拒绝、AKAZE fallback、描述子缓存 round-trip、PNG 调试图和静态 P95；2026-09-14 显式环境变量指向本地缓存图片时，Rust Windows decoder + OpenCV smoke 实测奶龙 `NAILONG 0.980`、奶蛙 `NAIWA_FROG 0.980`，OTHER 图片返回 `OTHER`，真实静态 P95 `1.3004ms`。本地 304 条验证 manifest（奶龙 178、奶蛙 4、OTHER 122）重新处理 304、跳过 0、奶龙正确 1、奶蛙正确 2、OTHER/UNKNOWN 122、`false_target_label=0`、`false_recall=0`；真实动画为 39 帧抽样 12 帧，P95 `154.3228ms`，未达到奶蛙严格门槛。另有回归测试确认动画参考图和查询图使用相同采样策略。该证据不是完整准确率验收，也不是训练数据或发布包内容。 |
| Windows NSIS 本机包 | 2026-09-13 OpenCV 专用 Tauri release build 与 NSIS 产物退出码 0；最终安装包 18,406,382 bytes，SHA-256 `4B1BBAF9C004692057A38BA6FA7413ABE2D52AA674074C2F098F7C5A32C6FBE1`。隔离目录安装后包含 `nlnf-desktop.exe`、`opencv_world4130.dll` 和 `uninstall.exe`；静默安装退出码 0；实际启动 `tauri.localhost` UI，显示初始化向导和离线分类边界；此前同一运行时构建已完成两类参考图添加、奶龙/奶蛙/负例三张识别，随后卸载退出码 0，安装目录和隔离应用数据均消失。最新提交 `e4db79b` 的 OpenCV artifact 也已在未安装隔离目录直接启动，窗口响应正常，并通过真实 Tauri UI 对现有参考图的奶龙/奶蛙普通分类（各 98%）；该自匹配使用 pHash 短路，不构成 QQ 撤回几何证据。仅代表当前机器，未覆盖第二台机器、Defender 或签名。 |
| 动图 | Rust decoder 已有采样边界，视觉层有多帧严格奶蛙门槛；真实 GIF smoke 已验证解码和性能，但仍缺真实奶蛙动画正例、Animated WebP 和完整 QQ 场景证据。 |
| 负例与鲁棒性 | 已有缩放、旋转、裁剪、JPEG 压缩、局部遮挡、文字覆盖、模糊、无关纹理拒绝、RANSAC 误匹配、低 inlier 高分拒绝、色彩辅助惩罚单测；同一份本地 304 条 manifest 重新验证后 `false_target_label=0`、`false_recall=0`，但仍缺真实角色冻结负例和更广泛正例覆盖。 |
| QQ | OneBot 11 已接入桌面后台 worker：仅允许 loopback、Token 只在内存、反向事件有界读取、群模式持久化、Observe 日志持久化/回读；本机 mock OneBot E2E 已通过。普通构建现在硬性拒绝 `AUTO_RECALL`，带 release 特性的构建仍必须先通过冻结验证门禁；真实 QQ/OneBot 账号和人工 Observe 运行仍未验证，因此 AUTO_RECALL 仍不作为发布能力开放。 |
| 冻结验证门禁 | `tools/validation/` 提供不复制图片的 JSONL manifest 生成器和本地 release-gate runner；Rust 门禁会校验 100/100/1000 类别数量、50 个真实 GIF、文件完整性、目标类漏检、目标类互相误判、负例误检和严格 false recall。每类可传入 1~10 张参考图；通过后生成绑定 Git revision、完整 Reference Bank 精确字节、manifest、视觉指纹、精确 OpenCV runtime DLL 和阈值指纹的 certificate；当前尚未提供满足门槛且 truth-reviewed 的冻结材料。 |

## 明确未完成

| 范围 | 状态 |
| --- | --- |
| OpenCV 原生环境 | 用户态工具目录和公开 CI 均已用固定 OpenCV 4.13.0 + LLVM 20.1.8 验证；提交 `e3a055af9c9104608f28a5d0e3af9322c2c9f616` 的 CI run `34965613779` 六项 job 全部通过，OpenCV NSIS、runtime staging 和 artifact 均成功；开发环境和 OpenCV 专用 NSIS staging 已可复现，第二台机器、Defender 和签名仍需独立验证。 |
| `OTHER` / `UNKNOWN` | 已区分：低分且两类均无几何 inlier 返回 `OTHER`；模糊或有部分证据但不足以确定时返回 `UNKNOWN`。仍需真实负例冻结集校准边界。 |
| 公开 GitHub | `AceYKN/nailoong-naiwa` 已确认公开；提交 `e3a055af9c9104608f28a5d0e3af9322c2c9f616` 的 GitHub CI run `34965613779` 中，仓库卫生、前端、Rust Ubuntu、Rust Windows、Windows Tauri 打包和 Windows OpenCV 原生 feature job 全部成功。 |
| DeepSeek key | 不再调用或上传图片；此前暴露过旧 key，用户应在服务商侧撤销。新 key 只要仍在本机用户环境变量中，也建议完成迁移后清除。 |

## 重跑核心验证

```powershell
cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml -- --check
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --all-targets
cargo clippy --manifest-path apps/desktop/src-tauri/Cargo.toml --all-targets -- -D warnings
pnpm --dir apps/desktop typecheck
pnpm --dir apps/desktop build
```

OpenCV feature build（需先按 `tools/feature_match/README.md` 设置用户态依赖变量）：

```powershell
cargo check --manifest-path apps/desktop/src-tauri/Cargo.toml --features opencv-backend
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --features opencv-backend --all-targets -- --test-threads=1
# auto-recall-release 需要先由 tools/validation 生成 certificate 并设置：
# $env:NLNF_VALIDATION_CERTIFICATE_JSON = [IO.File]::ReadAllText(...)
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --features opencv-backend,auto-recall-release --all-targets
cargo clippy --manifest-path apps/desktop/src-tauri/Cargo.toml --features opencv-backend,auto-recall-release --all-targets -- -D warnings
```
