# 奶龙 / 奶蛙参考图特征匹配与 QQ 自动处理系统

## Specification v2.0 — Training-Free Edition

本文件是当前实现的权威规格。它取代 v1 中“数据集采集 → 人工标注 → 训练 → ONNX → 模型迭代”的产品链路。

## 1. 产品定位

这是一个 Windows 本地桌面程序，通过传统计算机视觉参考图匹配识别：

- `NAILONG`：明确匹配奶龙
- `NAIWA_FROG`：明确匹配奶蛙
- `OTHER`：有足够证据认为不是目标角色
- `UNKNOWN`：证据不足或结果模糊

`OTHER` 和 `UNKNOWN` 在 QQ 决策中都等价于不撤回。

系统不要求大规模数据集、训练服务器、GPU、PyTorch、训练脚本、模型权重或 ONNX。参考图是类别定义，不是训练数据。

## 2. Reference Bank

目录和数据库都必须支持每类 `1~10` 张参考图：

```text
references/
├── nailong/
│   ├── NL01.png
│   └── NL02.png
└── naiwa_frog/
    ├── NF01.png
    └── NF02.png
```

一类一张是合法 MVP；架构不能写死为一张。建议每类 3~5 张，覆盖正脸、侧脸、全身、特写和典型表情。新增或删除参考图立即递增 `reference_set_version`，使预测缓存失效。

## 3. 视觉管线

```text
decode → 尺寸限制/保比例缩放 → pHash 粗筛
      → SIFT 或 AKAZE 特征提取
      → BF/FLANN KNN(k=2)
      → Lowe Ratio Test
      → RANSAC Homography
      → inliers / ratio / coverage / reprojection error
      → MatchResult → Decision Engine
```

OpenCV 是生产视觉后端；Rust/Tauri 桌面端直接调用它。颜色和 HSV 只能作辅助，不能单独决定类别。pHash 只负责同图/近似图快速命中，不能替代局部特征和几何验证。

静态图片必须支持 JPEG、PNG、WebP、GIF；BMP/AVIF 可作为后续扩展。GIF/Animated WebP 均匀抽取最多 8~12 帧，普通识别取最高有效帧分数；QQ 自动撤回至少要求两个采样帧通过严格几何门槛。

## 4. MatchResult 与评分

每个参考图匹配产生：

```text
referenceId, class
keypointCount, goodMatchCount, inlierCount
inlierRatio, coverage, reprojectionError
phashDistance, score
```

类别得分为该类别参考图得分的最大值。综合分必须同时考虑匹配强度、inlier ratio、空间 coverage 和 pHash 相似度。初始阈值可配置，未通过验证前不能宣称精度或开放自动撤回。

普通识别要求：

```text
max(nailongScore, naiwaScore) >= T_MATCH
winner - loser >= MIN_MARGIN
```

否则返回 `UNKNOWN`。QQ `AUTO_RECALL` 还必须满足更高的 `T_RECALL`、最小 inlier 数、最小 inlier ratio、最小 coverage、严格 margin 和 `VERY_HIGH` confidence。

## 5. 桌面架构

```text
React UI → Tauri command bridge → Rust VisionEngine → OpenCV
                                      ├─ ReferenceManager
                                      ├─ DecisionEngine
                                      ├─ PredictionCache
                                      └─ QQAdapter
```

页面保留：识别、QQ、参考图、设置。Feedback 页面不再作为训练闭环；识别失败时提供“添加为奶龙参考”或“添加为奶蛙参考”，保存后立即生效。

ReferenceManager 负责添加、删除、预计算和加载参考图的 pHash、关键点、描述子和 SHA-256。`references`、`prediction_cache`、`qq_groups`、`moderation_log`、`settings` 是主要 SQLite 表。

## 6. QQ 安全边界

保留 `OFF`、`OBSERVE`、`AUTO_RECALL`，默认 `OFF`。一条多图片消息任意图片强匹配奶蛙时最多撤回一次。Adapter 断线、无权限、图片获取失败、几何证据不足或重复事件都必须 fail closed。Token 不写日志，本地 QQ 服务仅允许 localhost，图片默认不长期保存。

## 7. 验收

### v0.1

- 每类添加 1 张参考图即可离线识别奶龙、奶蛙、其他/无法确定。
- 支持 JPEG/PNG/WebP/GIF 和基础 pHash、SIFT/AKAZE、Lowe Ratio、RANSAC 决策。
- 参考图缺失时不生成假结果。

### v0.2

- 支持每类 1~10 张参考图、参考图管理、缓存版本化、GIF/WebP 多帧和开发者匹配可视化。
- 新增参考图无需训练或重启即可生效。

### QQ

- OBSERVE 记录 `WOULD_RECALL` 但不撤回。
- AUTO_RECALL 只接受严格几何证据和 `VERY_HIGH` 结果，并通过 Mock、重复事件、断线和撤回失败测试。

### 测试

测试重点是传统视觉鲁棒性：缩放、旋转、裁剪、遮挡、JPEG 压缩、文字覆盖、GIF 采样、pHash 命中、RANSAC 排除错误匹配，以及黄色卡通人物/恐龙/皮卡丘/普通照片等负例。测试集是验证材料，不是训练集。

### 性能

目标为静态图 P95 < 200ms、GIF P95 < 1000ms；参考图数量增长时先用 pHash/辅助特征筛选 Top-K，再运行局部特征匹配。

## 8. 明确废弃

以下不再是产品实现路径，完成替代实现后应从构建、CI、发布和用户文档中移除：

- 大规模 QQ 数据集采集与人工标注门槛
- PyTorch/MobileNet 训练和校准
- ONNX 导出、Windows ML 模型包和模型迭代
- 以反馈数据训练模型的闭环
- DeepSeek 或其他云端图片预标注
