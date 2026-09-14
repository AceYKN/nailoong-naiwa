# Development Roadmap

目标是完成 [Specification v2.0](../spec-v2.md) 的零训练参考图匹配系统。只有存在可复现证据的项目才标记完成；旧 v1 的训练、标注和 ONNX 阶段不再作为路线图。

## Phase 1 — Vision contract

- [x] 固化 `NAILONG`、`NAIWA_FROG`、`OTHER`、`UNKNOWN` 标签与模糊区
- [x] 固化 1~10 张/类 Reference Bank 约束
- [x] Rust 决策层：class max score、margin、confidence 和 QQ recall gates
- [x] MatchResult 字段：good matches、inliers、ratio、coverage、重投影误差和 pHash 距离
- [x] OpenCV/Clang Windows 开发依赖与用户态安装路径已写入工具文档并在当前机器验证

## Phase 2 — Classical Vision Engine

- [x] pHash：Rust 实现、参考图元数据保存和查询距离计算
- [x] 将 pHash 接入候选粗筛并持久化描述子
- [x] SIFT 主路径源码与 feature-gated OpenCV 后端
- [x] AKAZE 备用路径（SIFT 无可用关键点/描述子时回退到 OpenCV AKAZE；按描述子类型选择 L2/Hamming 匹配，描述子缓存格式升级到 v4）
- [x] BFMatcher KNN(k=2) 与 Lowe Ratio Test 源码
- [x] RANSAC Homography、inliers、空间 coverage 和 reprojection error 源码
- [x] 静态 JPEG/PNG/WebP/GIF 统一解码边界
- [x] GIF/Animated WebP 的采样边界与多帧严格门槛
- [x] 开发者模式输出匹配点与几何调试图
- [x] 静态图 P95 < 200ms、GIF P95 < 1000ms 的本机真实 smoke 基准（2026-09-14 实测静态 1.3004ms、动画 154.3228ms；仍需第二台机器复测）

## Phase 3 — ReferenceManager and cache

- [x] 参考图初始化向导：识别页提供奶龙/奶蛙两步引导，每类至少 1 张即可开始
- [x] 添加、删除、多选导入和数量上限 10 张（桌面 UI 已接通并加载缩略图）
- [x] 保存 SHA-256、pHash、关键点、描述子、尺寸和来源元数据（描述子缓存为版本化二进制）
- [x] SQLite `reference_images`、`settings`、`prediction_cache` 和消息幂等表的 v5 schema
- [x] `reference_set_version` 在添加/删除时递增，且与 SQLite 参考图变更保持同一事务
- [x] 缓存键采用 `image_sha256 + reference_set_version + engine_fingerprint`；旧缓存迁移后不会被复用
- [x] 描述子缓存绑定实际参考图 SHA-256、提取器指纹和版本化二进制格式；Reference Bank 哈希在 QQ 安全门禁前重新读取并校验文件字节
- [x] 新增参考图不需要训练或重启即可更新数据库版本

## Phase 4 — Desktop MVP (v0.1)

- [x] 用 VisionEngine 作为桌面端分类路径（OpenCV feature build 已验证）
- [x] 识别页支持拖拽、批量上传、结果卡片和 UNKNOWN fail-closed
- [x] 参考图页提供每类 1~10 张管理与按剩余名额多选导入
- [x] 识别失败时可将当前图片立即加入指定 Reference Bank
- [x] 结果卡片显示 MatchResult 技术指标
- [x] 全流程离线 smoke test：隔离 Tauri/WebView 与最新 NSIS 包均完成 1 张奶龙 + 1 张奶蛙 + 1 张负例，结果为奶龙 98%、奶蛙 98%、OTHER

## Phase 5 — Animated and robustness (v0.2)

- [x] 多帧最高有效分普通识别与至少两个严格奶蛙帧的撤回门槛
- [x] 缩放鲁棒性、无关纹理拒绝、低 inlier 高分拒绝和色彩辅助惩罚测试
- [x] 旋转、裁剪、压缩、文字覆盖、模糊和局部遮挡测试（自生成纹理回归；真实角色遮挡仍需冻结样本）
- [ ] 黄色卡通、黄色恐龙、皮卡丘、普通照片等冻结负例集
- [x] RANSAC 错误匹配排除单测

## Phase 6 — QQ Mock and Observe

- [x] 保留 OFF / OBSERVE / AUTO_RECALL 的 Adapter 边界
- [x] 保留 MockQQAdapter、消息级 at-most-once 和 fail-closed 单元测试
- [x] 将 Mock/QQ 输入改接完整 `ClassificationResult`，Auto Recall 不再依赖单一分数
- [x] 单元级 OBSERVE 只记录 `WOULD_RECALL`，不产生副作用，并保留有界内存事件日志
- [x] OneBot loopback Adapter 的桌面连接、反向事件、OBSERVE 日志持久化/回读（本机 mock OneBot E2E）
- [ ] 用户真实 QQ/OneBot 账号的人工 Observe 运行；Adapter 仍仅限 localhost 且默认关闭

## Phase 7 — Auto Recall release gate

- [x] 将 `VERY_HIGH`、严格 margin、inliers、ratio、coverage、reprojection 和动画多帧条件固化为可执行决策门禁
- [x] 提供独立于运行时的冻结验证 manifest 生成器与 release-gate runner；未满足数据门槛时 fail closed
- [x] 普通构建默认拒绝 `AUTO_RECALL`，旧数据库配置自动降级为 `OBSERVE`；只有显式 `auto-recall-release` 特性构建才可进入后续发布门禁
- [x] release feature 必须嵌入冻结验证 certificate，并在运行时绑定完整 Reference Bank、manifest/视觉/缓存指纹、精确 OpenCV runtime、阈值和 Token；失配自动降级为 `OBSERVE`
- [ ] `VERY_HIGH`、严格 margin、inliers、ratio、coverage、reprojection 全部通过
- [ ] 100+ 奶龙、100+ 奶蛙、1000+ 其他、50+ GIF 的冻结验证材料
- [x] Mock 重复事件、断线、图片获取失败和撤回失败测试（`qq.rs` 覆盖 at-most-once、offline、download/classifier failure、recall failure）
- [x] 统计 false recall count，并将非零结果写入 certificate 生成门禁；未有证据前保持 AUTO_RECALL 禁用

## Phase 8 — Packaging and repository release

- [x] Windows 打包流程携带或明确检查 OpenCV runtime DLL（2026-09-13 NSIS release 构建、SHA-256、隔离安装/卸载均通过；正式标识符包的启动仍不在本次隔离检查范围）；公共 CI 现执行 OpenCV NSIS 构建并检查 exe、DLL 和安装包资源
- [x] 公共 CI 提供可复现且固定版本的 Windows OpenCV/Clang 原生 feature job（OpenCV 4.13.0 + LLVM 20.1.8；当前公开提交 `fde096fb9ebda46b2ceb8ed6c10a3d5894e43072` 的 CI run `34856737853` 已通过 check/test/clippy、Tauri OpenCV NSIS、runtime staging 与资源检查）；默认便携 job 仍不依赖机器专属原生工具链
- [x] 成功的 OpenCV CI run 上传短期 Windows 验证 artifact（NSIS 安装包、exe 和匹配 runtime DLL；保留 14 天，不等同于正式 Auto Recall release）
- [x] 安装、启动、识别页和参考图页本机验证（最新 NSIS 包在独立安装目录和独立应用数据目录启动；完成初始化向导、参考图添加、三张识别及卸载）
- [x] 从构建、CI、发布文档和当前 checkout 中移除 v1 的 PyTorch、训练、ONNX、DeepSeek 运行依赖
- [x] 清理旧 Git 历史中的本地敏感标识后，创建干净公开 GitHub 初始提交
- [x] 用户确认后创建并推送公开仓库 `AceYKN/nailoong-naiwa`；正式 Auto Recall release gate 仍未通过

## 明确删除项

不再开发以下链路：QQ 数据集采集门槛、人工批量标注、PyTorch/MobileNet 训练、ONNX 导出、Windows ML 模型包、反馈训练闭环、DeepSeek 图片预标注。上述 v1 代码已从当前 checkout 删除；用户本地 QQ 缓存图片不属于仓库迁移范围，也未被删除。
