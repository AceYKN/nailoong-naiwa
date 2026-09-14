import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  addReference,
  classifyImage,
  connectQq,
  debugMatchImage,
  disconnectQq,
  getAppInfo,
  getQqStatus,
  inspectImage,
  listReferences,
  readReference,
  removeReference,
  setQqGroupMode,
} from "./services/appBridge";
import type {
  AppInfo,
  AppView,
  ClassificationLabel,
  ClassificationResult,
  ConfidenceLevel,
  ImageInspection,
  QueuedImage,
  QqMode,
  QqStatus,
  ReferenceClass,
  ReferenceInfo,
} from "./types/domain";
import { DISPLAY_NAMES, REFERENCE_NAMES } from "./types/domain";

const ACCEPTED_TYPES = ["image/png", "image/jpeg", "image/webp", "image/gif"];
const MAX_FILE_SIZE_BYTES = 25 * 1024 * 1024;
const MAX_WIDTH = 8192;
const MAX_HEIGHT = 8192;
const MAX_PIXELS = 50_000_000;
const MAX_QUEUE_SIZE = 120;
const MAX_REFERENCES_PER_CLASS = 10;

const fallbackQqStatus: QqStatus = {
  connected: false,
  actionEndpoint: null,
  eventEndpoint: null,
  tokenConfigured: false,
  autoRecallAvailable: false,
  groups: [],
  recentEvents: [],
  lastError: null,
};

const fallbackAppInfo: AppInfo = {
  productName: "NLNF Classifier",
  appVersion: "0.1.0",
  phase: "Phase 4 — Reference matching",
  visionBackend: "opencv-sift-unavailable",
  visionAvailable: false,
  visionMessage: "请在 Tauri 桌面程序中编译并启用 OpenCV SIFT 后端",
  referenceSetVersion: 1,
  nailongReferenceCount: 0,
  naiwaFrogReferenceCount: 0,
  networkRequiredForClassification: false,
};

function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function isAccepted(file: File): boolean {
  return ACCEPTED_TYPES.includes(file.type) || /\.(png|jpe?g|webp|gif)$/i.test(file.name);
}

function browserFormat(file: File): ImageInspection["format"] {
  if (file.type === "image/png" || /\.png$/i.test(file.name)) return "PNG";
  if (file.type === "image/webp" || /\.webp$/i.test(file.name)) return "WEBP";
  if (file.type === "image/gif" || /\.gif$/i.test(file.name)) return "GIF";
  return "JPEG";
}

function referenceMime(path: string): string {
  if (/\.png$/i.test(path)) return "image/png";
  if (/\.webp$/i.test(path)) return "image/webp";
  if (/\.gif$/i.test(path)) return "image/gif";
  return "image/jpeg";
}

async function sha256Hex(bytes: ArrayBuffer): Promise<string> {
  const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function inspectInBrowser(file: File, bytes: ArrayBuffer): Promise<ImageInspection> {
  return new Promise<ImageInspection>((resolve, reject) => {
    const previewUrl = URL.createObjectURL(file);
    const image = new Image();
    image.onload = () => {
      URL.revokeObjectURL(previewUrl);
      if (image.naturalWidth > MAX_WIDTH || image.naturalHeight > MAX_HEIGHT) {
        reject(new Error(`图片尺寸 ${image.naturalWidth}×${image.naturalHeight} 超过 8192×8192 限制`));
        return;
      }
      if (image.naturalWidth * image.naturalHeight > MAX_PIXELS) {
        reject(new Error("图片解码像素数超过 50,000,000 限制"));
        return;
      }
      resolve({
        bytes: file.size,
        sha256: "",
        format: browserFormat(file),
        width: image.naturalWidth,
        height: image.naturalHeight,
        animated: false,
        frameCount: 1,
      });
    };
    image.onerror = () => {
      URL.revokeObjectURL(previewUrl);
      reject(new Error("图片无法解码或格式不受支持"));
    };
    image.src = previewUrl;
  }).then(async (inspection) => ({ ...inspection, sha256: await sha256Hex(bytes) }));
}

async function inspectLocalFile(file: File): Promise<ImageInspection> {
  if (file.size > MAX_FILE_SIZE_BYTES) {
    throw new Error("图片文件超过 25 MiB 限制");
  }
  const bytes = await file.arrayBuffer();
  if (isTauriRuntime()) {
    return inspectImage(new Uint8Array(bytes));
  }
  return inspectInBrowser(file, bytes);
}

function viewTitle(view: AppView): string {
  return {
    identify: "图片识别",
    qq: "QQ 连接",
    references: "参考图",
    settings: "设置",
  }[view];
}

function labelText(label: ClassificationLabel): string {
  return DISPLAY_NAMES[label];
}

function confidenceText(level: ConfidenceLevel): string {
  return {
    NONE: "无",
    LOW: "低",
    MEDIUM: "中",
    HIGH: "高",
    VERY_HIGH: "极高",
  }[level];
}

function percent(value: number): string {
  return `${(value * 100).toFixed(1)}%`;
}

function compactBytes(value: number): string {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KiB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MiB`;
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function referenceClassLabel(referenceClass: ReferenceClass): string {
  return REFERENCE_NAMES[referenceClass];
}

function formatReferenceDate(value: string): string {
  if (value.startsWith("unix:")) {
    const seconds = Number(value.slice(5));
    if (Number.isFinite(seconds)) return new Date(seconds * 1000).toLocaleString();
  }
  return value;
}

function resultTone(result: ClassificationResult): string {
  if (result.label === "NAIWA_FROG") return "frog";
  if (result.label === "NAILONG") return "nailong";
  return "uncertain";
}

function App() {
  const [view, setView] = useState<AppView>("identify");
  const [appInfo, setAppInfo] = useState<AppInfo>(fallbackAppInfo);
  const [references, setReferences] = useState<ReferenceInfo[]>([]);
  const [images, setImages] = useState<QueuedImage[]>([]);
  const [dragging, setDragging] = useState(false);
  const [inputError, setInputError] = useState<string | null>(null);
  const [referenceError, setReferenceError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [loadingReferences, setLoadingReferences] = useState(false);
  const [addingClass, setAddingClass] = useState<ReferenceClass | null>(null);
  const [qqStatus, setQqStatus] = useState<QqStatus>(fallbackQqStatus);
  const [qqActionEndpoint, setQqActionEndpoint] = useState("http://127.0.0.1:5700/");
  const [qqEventEndpoint, setQqEventEndpoint] = useState("http://127.0.0.1:5701/");
  const [qqToken, setQqToken] = useState("");
  const [qqBusy, setQqBusy] = useState(false);
  const [qqError, setQqError] = useState<string | null>(null);
  const [batchProgress, setBatchProgress] = useState<{ done: number; total: number } | null>(null);
  const batchClassifyingRef = useRef(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const nailongReferenceInputRef = useRef<HTMLInputElement>(null);
  const frogReferenceInputRef = useRef<HTMLInputElement>(null);
  const onboardingNailongInputRef = useRef<HTMLInputElement>(null);
  const onboardingFrogInputRef = useRef<HTMLInputElement>(null);
  const imagesRef = useRef<QueuedImage[]>([]);
  const referencePreviewUrls = useRef(new Map<string, string>());

  const refreshAppState = useCallback(async () => {
    if (!isTauriRuntime()) return;
    const [info, bank] = await Promise.all([getAppInfo(), listReferences()]);
    referencePreviewUrls.current.forEach((url) => URL.revokeObjectURL(url));
    referencePreviewUrls.current.clear();
    const enrichedBank = await Promise.all(bank.map(async (reference) => {
      try {
        const bytes = await readReference(reference.id);
        const blobBytes = new ArrayBuffer(bytes.byteLength);
        new Uint8Array(blobBytes).set(bytes);
        const previewUrl = URL.createObjectURL(new Blob([blobBytes], { type: referenceMime(reference.filePath) }));
        referencePreviewUrls.current.set(reference.id, previewUrl);
        return { ...reference, previewUrl };
      } catch {
        return reference;
      }
    }));
    setAppInfo(info);
    setReferences(enrichedBank);
  }, []);

  useEffect(() => {
    void refreshAppState().catch((error) => setInputError(errorText(error)));
  }, [refreshAppState]);

  const refreshQqStatus = useCallback(async () => {
    if (!isTauriRuntime()) return;
    const status = await getQqStatus();
    setQqStatus(status);
  }, []);

  useEffect(() => {
    void refreshQqStatus().catch((error) => setQqError(errorText(error)));
    const timer = window.setInterval(() => {
      void refreshQqStatus().catch((error) => setQqError(errorText(error)));
    }, 3000);
    return () => window.clearInterval(timer);
  }, [refreshQqStatus]);

  useEffect(() => {
    imagesRef.current = images;
  }, [images]);

  useEffect(() => () => {
    imagesRef.current.forEach((image) => {
      URL.revokeObjectURL(image.previewUrl);
      if (image.debugPreviewUrl) URL.revokeObjectURL(image.debugPreviewUrl);
    });
    referencePreviewUrls.current.forEach((url) => URL.revokeObjectURL(url));
  }, []);

  const referencesByClass = useMemo(
    () => ({
      NAILONG: references.filter((reference) => reference.class === "NAILONG"),
      NAIWA_FROG: references.filter((reference) => reference.class === "NAIWA_FROG"),
    }),
    [references],
  );

  const addFiles = useCallback(async (files: File[]) => {
    setInputError(null);
    setNotice(null);
    const accepted = files.filter(isAccepted);
    const remainingSlots = Math.max(0, MAX_QUEUE_SIZE - images.length);
    const selected = accepted.slice(0, remainingSlots);
    const rejectedCount = files.length - accepted.length;
    const issues: string[] = [];
    if (rejectedCount > 0) issues.push(`${rejectedCount} 个文件不是支持的图片格式`);
    if (accepted.length > selected.length) issues.push(`当前批次最多保留 ${MAX_QUEUE_SIZE} 张图片`);

    const next: QueuedImage[] = [];
    for (const [index, file] of selected.entries()) {
      try {
        const inspection = await inspectLocalFile(file);
        next.push({
          id: `${file.name}-${file.lastModified}-${index}-${Date.now()}`,
          file,
          previewUrl: URL.createObjectURL(file),
          inspection,
        });
      } catch (error) {
        issues.push(`${file.name}: ${errorText(error)}`);
      }
    }
    if (next.length > 0) setImages((current) => [...current, ...next]);
    setInputError(issues.length > 0 ? issues.join("；") : null);
  }, [images.length]);

  const classify = useCallback(async (id: string) => {
    if (!isTauriRuntime()) {
      setImages((current) => current.map((image) => image.id === id
        ? { ...image, classificationError: "浏览器预览不执行识别，请在 Tauri 桌面程序中运行" }
        : image));
      return;
    }
    if (!appInfo.visionAvailable) {
      setImages((current) => current.map((image) => image.id === id
        ? { ...image, classificationError: appInfo.visionMessage }
        : image));
      return;
    }
    const image = images.find((candidate) => candidate.id === id);
    if (!image) return;
    setImages((current) => current.map((candidate) => candidate.id === id
      ? {
          ...candidate,
          classifying: true,
          classificationError: undefined,
          debugError: undefined,
          ...(candidate.debugPreviewUrl ? { debugPreviewUrl: undefined } : {}),
        }
      : candidate));
    if (image.debugPreviewUrl) URL.revokeObjectURL(image.debugPreviewUrl);
    try {
      const result = await classifyImage(new Uint8Array(await image.file.arrayBuffer()));
      setImages((current) => current.map((candidate) => candidate.id === id
        ? { ...candidate, classifying: false, classification: result }
        : candidate));
    } catch (error) {
      setImages((current) => current.map((candidate) => candidate.id === id
        ? { ...candidate, classifying: false, classificationError: errorText(error) }
        : candidate));
    }
  }, [appInfo.visionAvailable, appInfo.visionMessage, images]);

  const classifyPending = useCallback(async () => {
    if (batchClassifyingRef.current) return;
    const pending = images.filter((image) => !image.classification && !image.classifying);
    if (pending.length === 0) return;

    batchClassifyingRef.current = true;
    setBatchProgress({ done: 0, total: pending.length });
    let cursor = 0;
    const worker = async () => {
      while (true) {
        const index = cursor;
        cursor += 1;
        if (index >= pending.length) return;
        await classify(pending[index].id);
        setBatchProgress((progress) => progress ? { ...progress, done: progress.done + 1 } : progress);
      }
    };
    try {
      await Promise.all(Array.from({ length: Math.min(2, pending.length) }, () => worker()));
    } finally {
      batchClassifyingRef.current = false;
      setBatchProgress(null);
    }
  }, [classify, images]);

  const showDebugMatch = useCallback(async (id: string) => {
    if (!isTauriRuntime()) return;
    const image = images.find((candidate) => candidate.id === id);
    const referenceId = image?.classification?.bestMatch?.referenceId;
    if (!image || !referenceId) return;
    setImages((current) => current.map((candidate) => candidate.id === id
      ? { ...candidate, debugLoading: true, debugError: undefined }
      : candidate));
    if (image.debugPreviewUrl) URL.revokeObjectURL(image.debugPreviewUrl);
    try {
      const bytes = await debugMatchImage(new Uint8Array(await image.file.arrayBuffer()), referenceId);
      const blobBytes = new ArrayBuffer(bytes.byteLength);
      new Uint8Array(blobBytes).set(bytes);
      const debugPreviewUrl = URL.createObjectURL(new Blob([blobBytes], { type: "image/png" }));
      setImages((current) => current.map((candidate) => candidate.id === id
        ? { ...candidate, debugLoading: false, debugPreviewUrl }
        : candidate));
    } catch (error) {
      setImages((current) => current.map((candidate) => candidate.id === id
        ? { ...candidate, debugLoading: false, debugError: errorText(error) }
        : candidate));
    }
  }, [images]);

  const removeImage = useCallback((id: string) => {
    setImages((current) => {
      const image = current.find((candidate) => candidate.id === id);
      if (image) {
        URL.revokeObjectURL(image.previewUrl);
        if (image.debugPreviewUrl) URL.revokeObjectURL(image.debugPreviewUrl);
      }
      return current.filter((candidate) => candidate.id !== id);
    });
  }, []);

  const clearImages = useCallback(() => {
    images.forEach((image) => {
      URL.revokeObjectURL(image.previewUrl);
      if (image.debugPreviewUrl) URL.revokeObjectURL(image.debugPreviewUrl);
    });
    setImages([]);
    setInputError(null);
  }, [images]);

  const handleFileInput = useCallback((event: React.ChangeEvent<HTMLInputElement>) => {
    const files = event.currentTarget.files ? Array.from(event.currentTarget.files) : [];
    event.currentTarget.value = "";
    void addFiles(files);
  }, [addFiles]);

  const handleDrop = useCallback((event: React.DragEvent<HTMLDivElement>) => {
    event.preventDefault();
    setDragging(false);
    void addFiles(Array.from(event.dataTransfer.files));
  }, [addFiles]);

  const handleReferenceFiles = useCallback(async (referenceClass: ReferenceClass, files: File[]) => {
    if (files.length === 0) return;
    setReferenceError(null);
    setNotice(null);
    if (!isTauriRuntime()) {
      setReferenceError("浏览器预览不能写入本地参考库，请在 Tauri 桌面程序中运行");
      return;
    }
    const accepted = files.filter(isAccepted);
    const rejectedCount = files.length - accepted.length;
    const remainingSlots = Math.max(0, MAX_REFERENCES_PER_CLASS - referencesByClass[referenceClass].length);
    if (remainingSlots === 0) {
      setReferenceError(`${referenceClassLabel(referenceClass)}参考图已达到上限 ${MAX_REFERENCES_PER_CLASS} 张`);
      return;
    }
    const selected = accepted.slice(0, remainingSlots);
    const issues: string[] = [];
    if (rejectedCount > 0) issues.push(`${rejectedCount} 个文件不是支持的图片格式`);
    if (accepted.length > selected.length) {
      issues.push(`本次只写入剩余 ${remainingSlots} 个参考图名额`);
    }

    setAddingClass(referenceClass);
    let addedCount = 0;
    for (const file of selected) {
      try {
        await addReference(referenceClass, new Uint8Array(await file.arrayBuffer()));
        addedCount += 1;
      } catch (error) {
        issues.push(`${file.name}: ${errorText(error)}`);
      }
    }
    try {
      await refreshAppState();
    } catch (error) {
      issues.push(`参考库刷新失败：${errorText(error)}`);
    }
    if (addedCount > 0) {
      setNotice(`${referenceClassLabel(referenceClass)}参考图已加入 ${addedCount} 张，新的参考库版本已生效`);
    }
    setReferenceError(issues.length > 0 ? issues.join("；") : null);
    setAddingClass(null);
  }, [referencesByClass, refreshAppState]);

  const handleRemoveReference = useCallback(async (reference: ReferenceInfo) => {
    if (!window.confirm(`确认删除参考图 ${reference.id}？`)) return;
    setReferenceError(null);
    setNotice(null);
    try {
      await removeReference(reference.id);
      await refreshAppState();
      setNotice(`${referenceClassLabel(reference.class)}参考图已删除`);
    } catch (error) {
      setReferenceError(errorText(error));
    }
  }, [refreshAppState]);

  const handleAddImageAsReference = useCallback(async (id: string, referenceClass: ReferenceClass) => {
    setNotice(null);
    setImages((current) => current.map((image) => image.id === id
      ? { ...image, referenceAddingClass: referenceClass, referenceError: undefined }
      : image));
    if (!isTauriRuntime()) {
      setImages((current) => current.map((image) => image.id === id
        ? { ...image, referenceAddingClass: undefined, referenceError: "浏览器预览不能写入本地参考库，请在 Tauri 桌面程序中运行" }
        : image));
      return;
    }
    const image = images.find((candidate) => candidate.id === id);
    if (!image) return;
    try {
      await addReference(referenceClass, new Uint8Array(await image.file.arrayBuffer()));
      await refreshAppState();
      setImages((current) => current.map((candidate) => candidate.id === id
        ? {
            ...candidate,
            referenceAddingClass: undefined,
            referenceAddedClasses: [...(candidate.referenceAddedClasses ?? []), referenceClass],
          }
        : candidate));
      setNotice(`${referenceClassLabel(referenceClass)}参考图已加入，新的参考库版本已生效`);
    } catch (error) {
      setImages((current) => current.map((candidate) => candidate.id === id
        ? { ...candidate, referenceAddingClass: undefined, referenceError: errorText(error) }
        : candidate));
    }
  }, [images, refreshAppState]);

  const renderResult = (result: ClassificationResult) => {
    const match = result.bestMatch;
    return (
      <div className={`classification-result ${resultTone(result)}`}>
        <strong>{labelText(result.label)}</strong>
        <span>奶龙 {percent(result.nailongScore)} · 奶蛙 {percent(result.naiwaFrogScore)}</span>
        <span>置信度 {confidenceText(result.confidenceLevel)} · 几何验证 {result.geometryValid ? "通过" : "未通过"}</span>
        {match && <span>Good {match.goodMatchCount} · Inliers {match.inlierCount} · 覆盖 {percent(match.coverage)}</span>}
      </div>
    );
  };

  const renderIdentify = () => (
    <>
      <section className="hero-card">
        <div>
          <p className="eyebrow">TRAINING-FREE VISION</p>
          <h1>奶龙 / 奶蛙识别</h1>
          <p className="hero-copy">
            使用本地参考图与 SIFT 特征匹配识别图片，不采集训练数据、不上传图片，分类过程完全离线。
          </p>
        </div>
        <div className="status-pill" data-status={appInfo.visionAvailable ? "ready" : "error"}>
          <span className={appInfo.visionAvailable ? "status-dot" : "offline-dot"} />
          <span>{appInfo.visionMessage}</span>
        </div>
      </section>

      {(appInfo.nailongReferenceCount === 0 || appInfo.naiwaFrogReferenceCount === 0) && (
        <div className="onboarding-card">
          <strong>初始化向导 · 先添加两类参考图</strong>
          <span>One-shot 合法：奶龙和奶蛙各放 1 张即可开始；识别遇到新姿态时再追加参考图。</span>
          <input
            ref={onboardingNailongInputRef}
            hidden
            type="file"
            accept={ACCEPTED_TYPES.join(",")}
            onChange={(event) => {
              const file = event.currentTarget.files?.[0];
              event.currentTarget.value = "";
              void handleReferenceFiles("NAILONG", file ? [file] : []);
            }}
          />
          <input
            ref={onboardingFrogInputRef}
            hidden
            type="file"
            accept={ACCEPTED_TYPES.join(",")}
            onChange={(event) => {
              const file = event.currentTarget.files?.[0];
              event.currentTarget.value = "";
              void handleReferenceFiles("NAIWA_FROG", file ? [file] : []);
            }}
          />
          <div className="onboarding-steps">
            <div className={`onboarding-step${appInfo.nailongReferenceCount > 0 ? " is-done" : ""}`}>
              <span><b>1</b> 奶龙参考图</span>
              {appInfo.nailongReferenceCount > 0 ? (
                <em>已完成 · {appInfo.nailongReferenceCount} 张</em>
              ) : (
                <button type="button" className="secondary-button" onClick={() => onboardingNailongInputRef.current?.click()} disabled={addingClass !== null}>
                  {addingClass === "NAILONG" ? "写入中…" : "选择图片"}
                </button>
              )}
            </div>
            <div className={`onboarding-step${appInfo.naiwaFrogReferenceCount > 0 ? " is-done" : ""}`}>
              <span><b>2</b> 奶蛙参考图</span>
              {appInfo.naiwaFrogReferenceCount > 0 ? (
                <em>已完成 · {appInfo.naiwaFrogReferenceCount} 张</em>
              ) : (
                <button type="button" className="secondary-button" onClick={() => onboardingFrogInputRef.current?.click()} disabled={addingClass !== null}>
                  {addingClass === "NAIWA_FROG" ? "写入中…" : "选择图片"}
                </button>
              )}
            </div>
          </div>
          {referenceError && <div className="input-issues onboarding-error"><span>{referenceError}</span></div>}
          <button type="button" className="text-button" onClick={() => setView("references")}>打开参考图库管理更多参考图</button>
        </div>
      )}

      <input ref={fileInputRef} hidden type="file" accept={ACCEPTED_TYPES.join(",")} multiple onChange={handleFileInput} />
      <div
        className={`drop-zone${dragging ? " is-dragging" : ""}`}
        onDragEnter={(event) => { event.preventDefault(); setDragging(true); }}
        onDragOver={(event) => event.preventDefault()}
        onDragLeave={(event) => { if (event.currentTarget === event.target) setDragging(false); }}
        onDrop={handleDrop}
      >
        <div>
          <div className="drop-icon">＋</div>
          <h2>把图片拖到这里</h2>
          <p>支持 PNG、JPEG、WebP、GIF；本地批次最多 {MAX_QUEUE_SIZE} 张</p>
          <button type="button" className="secondary-button" onClick={() => fileInputRef.current?.click()}>选择文件</button>
          <div className="format-note">图片只在本机处理</div>
        </div>
      </div>
      {inputError && <div className="input-issues"><span>{inputError}</span></div>}

      <div className="section-heading">
        <div><p className="eyebrow">REVIEW QUEUE</p><h2>待识别图片</h2></div>
        <div className="section-actions">
          <span className="count-badge">{images.length} / {MAX_QUEUE_SIZE}</span>
          {batchProgress && <span className="count-badge">匹配 {batchProgress.done}/{batchProgress.total}</span>}
          {images.some((image) => !image.classification && !image.classifying) && (
            <button type="button" className="secondary-button" onClick={() => void classifyPending()} disabled={batchProgress !== null}>
              {batchProgress ? "批量匹配中…" : "批量开始匹配"}
            </button>
          )}
          {images.length > 0 && <button type="button" className="text-button" onClick={clearImages} disabled={batchProgress !== null}>清空</button>}
        </div>
      </div>
      {images.length === 0 ? (
        <div className="empty-state"><div><span className="empty-mark">◌</span><p>还没有待识别图片</p><small>可以直接从 QQ 缓存目录选择图片</small></div></div>
      ) : (
        <div className="image-grid">
          {images.map((image) => (
            <article className="image-card" key={image.id}>
              <img src={image.previewUrl} alt={image.file.name} />
              <div className="image-card-body">
                <strong title={image.file.name}>{image.file.name}</strong>
                <span>{image.inspection.format} · {image.inspection.width}×{image.inspection.height} · {compactBytes(image.inspection.bytes)}</span>
                {image.classification && renderResult(image.classification)}
                {image.classification?.bestMatch && (
                  <button type="button" className="text-button" onClick={() => void showDebugMatch(image.id)} disabled={image.debugLoading}>
                    {image.debugLoading ? "生成特征图…" : "显示匹配特征点"}
                  </button>
                )}
                {image.debugPreviewUrl && <img className="debug-preview" src={image.debugPreviewUrl} alt="SIFT 与 RANSAC 匹配特征点" />}
                {image.debugError && <div className="card-error">特征图生成失败：{image.debugError}</div>}
                {image.classificationError && <div className="card-error">{image.classificationError}</div>}
                <div className="card-actions">
                  <button type="button" className="secondary-button" onClick={() => void classify(image.id)} disabled={image.classifying}>
                    {image.classifying ? "匹配中…" : "开始匹配"}
                  </button>
                  <button type="button" className="text-button" onClick={() => removeImage(image.id)}>移除</button>
                </div>
                <div className="reference-actions">
                  <span>加入参考库</span>
                  {(["NAILONG", "NAIWA_FROG"] as ReferenceClass[]).map((referenceClass) => {
                    const added = image.referenceAddedClasses?.includes(referenceClass) ?? false;
                    const busy = image.referenceAddingClass === referenceClass;
                    const full = referencesByClass[referenceClass].length >= 10;
                    return (
                      <button
                        type="button"
                        className="text-button"
                        key={referenceClass}
                        onClick={() => void handleAddImageAsReference(image.id, referenceClass)}
                        disabled={busy || added || full || image.referenceAddingClass !== undefined}
                      >
                        {busy ? "写入中…" : added ? `${referenceClassLabel(referenceClass)}已加入` : full ? `${referenceClassLabel(referenceClass)}已满` : referenceClassLabel(referenceClass)}
                      </button>
                    );
                  })}
                </div>
                {image.referenceError && <div className="card-error">{image.referenceError}</div>}
              </div>
            </article>
          ))}
        </div>
      )}
    </>
  );

  const renderReferences = () => (
    <div className="settings-page references-page">
      <p className="eyebrow">REFERENCE BANK</p>
      <div className="page-heading-row">
        <div><h1>参考图库</h1><p>每类允许 1–10 张参考图；添加或删除后会自动提升参考库版本。</p></div>
        <span className="count-badge">版本 {appInfo.referenceSetVersion}</span>
      </div>
      {referenceError && <div className="input-issues"><span>{referenceError}</span></div>}
      {notice && <div className="notice-box">{notice}</div>}
      <div className="reference-columns">
        {(["NAILONG", "NAIWA_FROG"] as ReferenceClass[]).map((referenceClass) => {
          const group = referencesByClass[referenceClass];
          const inputRef = referenceClass === "NAILONG" ? nailongReferenceInputRef : frogReferenceInputRef;
          return (
            <section className="reference-card" key={referenceClass}>
              <div className="section-heading compact-heading">
                <div><p className="eyebrow">{referenceClass}</p><h2>{referenceClassLabel(referenceClass)}</h2></div>
                <span className="count-badge">{group.length} / 10</span>
              </div>
              <input
                ref={inputRef}
                hidden
                type="file"
                multiple
                accept={ACCEPTED_TYPES.join(",")}
                onChange={(event) => {
                  const files = Array.from(event.currentTarget.files ?? []);
                  event.currentTarget.value = "";
                  void handleReferenceFiles(referenceClass, files);
                }}
              />
              <button type="button" className="secondary-button" onClick={() => inputRef.current?.click()} disabled={group.length >= MAX_REFERENCES_PER_CLASS || addingClass !== null}>
                {addingClass === referenceClass ? "写入中…" : "添加参考图（可多选）"}
              </button>
              {group.length === 0 ? (
                <div className="empty-state reference-empty"><p>尚未添加参考图</p><small>One-shot 可以直接开始；建议逐步补充到 3–5 张。</small></div>
              ) : (
                <div className="reference-list">
                  {group.map((reference) => (
                    <div className="reference-row" key={reference.id}>
                      {reference.previewUrl ? <img className="reference-thumb" src={reference.previewUrl} alt={`${referenceClassLabel(reference.class)}参考图`} /> : <div className="reference-thumb reference-thumb-empty">?</div>}
                      <div><strong>{reference.id}</strong><span>{reference.width}×{reference.height} · {formatReferenceDate(reference.createdAt)}</span></div>
                      <button type="button" className="text-button" onClick={() => void handleRemoveReference(reference)}>删除</button>
                    </div>
                  ))}
                </div>
              )}
            </section>
          );
        })}
      </div>
      <p className="settings-note">参考图会复制到应用数据目录；原始 QQ 缓存不会被移动或修改。识别结果只使用当前参考库，新增参考图后旧缓存会自动失效。</p>
    </div>
  );

  const handleConnectQq = useCallback(async () => {
    if (!isTauriRuntime()) {
      setQqError("浏览器预览不能连接 QQ，请在 Tauri 桌面程序中运行");
      return;
    }
    setQqBusy(true);
    setQqError(null);
    try {
      const status = await connectQq(qqActionEndpoint, qqEventEndpoint, qqToken);
      setQqStatus(status);
      setQqToken("");
    } catch (error) {
      setQqError(errorText(error));
    } finally {
      setQqBusy(false);
    }
  }, [qqActionEndpoint, qqEventEndpoint, qqToken]);

  const handleDisconnectQq = useCallback(async () => {
    setQqBusy(true);
    setQqError(null);
    try {
      setQqStatus(await disconnectQq());
    } catch (error) {
      setQqError(errorText(error));
    } finally {
      setQqBusy(false);
    }
  }, []);

  const handleQqModeChange = useCallback(async (group: QqStatus["groups"][number], mode: QqMode) => {
    if (mode === "AUTO_RECALL" && !qqStatus.autoRecallAvailable) {
      setQqError("AUTO_RECALL 尚未开放：请先通过冻结验证门禁并使用 release 特性构建");
      return;
    }
    if (mode === "AUTO_RECALL" && !window.confirm("AUTO_RECALL 会在严格几何证据通过后调用 QQ 撤回消息。确认开启这个群的自动撤回吗？")) {
      return;
    }
    setQqBusy(true);
    setQqError(null);
    try {
      setQqStatus(await setQqGroupMode(group.groupId, group.groupName, mode, group.recallThreshold));
    } catch (error) {
      setQqError(errorText(error));
    } finally {
      setQqBusy(false);
    }
  }, [qqStatus.autoRecallAvailable]);

  const renderQq = () => (
    <div className="settings-page qq-page">
      <p className="eyebrow">QQ ADAPTER</p>
      <div className="page-heading-row">
        <div><h1>QQ 连接</h1><p>仅允许连接本机 OneBot 11；图片在本地匹配，默认模式为 OFF。</p></div>
        <span className="count-badge" data-status={qqStatus.connected ? "ready" : "idle"}>{qqStatus.connected ? "已连接" : "未连接"}</span>
      </div>
      {qqError && <div className="input-issues"><span>{qqError}</span></div>}
      {qqStatus.lastError && !qqError && <div className="input-issues"><span>{qqStatus.lastError}</span></div>}
      <section className="settings-card qq-connection-card">
        <div className="settings-grid qq-endpoint-grid">
          <label><span>OneBot API 地址</span><input className="text-input" value={qqActionEndpoint} onChange={(event) => setQqActionEndpoint(event.target.value)} disabled={qqStatus.connected || qqBusy} /></label>
          <label><span>反向事件监听地址</span><input className="text-input" value={qqEventEndpoint} onChange={(event) => setQqEventEndpoint(event.target.value)} disabled={qqStatus.connected || qqBusy} /></label>
          <label><span>Access Token（仅内存）</span><input className="text-input" type="password" value={qqToken} onChange={(event) => setQqToken(event.target.value)} disabled={qqStatus.connected || qqBusy} autoComplete="off" /></label>
        </div>
        <div className="card-actions">
          {qqStatus.connected ? (
            <button type="button" className="secondary-button" onClick={() => void handleDisconnectQq()} disabled={qqBusy}>{qqBusy ? "断开中…" : "断开 QQ"}</button>
          ) : (
            <button type="button" className="secondary-button" onClick={() => void handleConnectQq()} disabled={qqBusy}>{qqBusy ? "连接中…" : "连接并开始监听"}</button>
          )}
          {qqStatus.tokenConfigured && <span className="format-note">Token 已配置但不会显示或写入日志</span>}
        </div>
      </section>
      <section className="settings-card">
        <div className="section-heading compact-heading"><div><p className="eyebrow">GROUP MODES</p><h2>群组安全策略</h2></div><span className="count-badge">{qqStatus.groups.length} 个群</span></div>
        <p className="settings-note">OFF 不处理图片；OBSERVE 只记录 WOULD_RECALL；AUTO_RECALL 还需要冻结验证门禁、release 特性和二次确认。当前状态：{qqStatus.autoRecallAvailable ? "已开放" : "未开放"}。</p>
        {qqStatus.groups.length === 0 ? <div className="empty-state reference-empty"><p>尚未发现 QQ 群</p><small>连接成功后会从 OneBot 读取群列表。</small></div> : (
          <div className="qq-group-list">
            {qqStatus.groups.map((group) => (
              <div className="qq-group-row" key={group.groupId}>
                <div><strong>{group.groupName || "未命名群"}</strong><span>{group.groupId} · 撤回阈值 {(group.recallThreshold * 100).toFixed(0)}%</span></div>
                <select value={group.mode} onChange={(event) => void handleQqModeChange(group, event.target.value as QqMode)} disabled={qqBusy}>
                  <option value="OFF">OFF</option>
                  <option value="OBSERVE">OBSERVE</option>
                  <option value="AUTO_RECALL" disabled={!qqStatus.autoRecallAvailable}>AUTO_RECALL{qqStatus.autoRecallAvailable ? "" : "（未开放）"}</option>
                </select>
              </div>
            ))}
          </div>
        )}
      </section>
      <section className="settings-card">
        <div className="section-heading compact-heading"><div><p className="eyebrow">OBSERVE LOG</p><h2>最近事件</h2></div><span className="count-badge">{qqStatus.recentEvents.length}</span></div>
        {qqStatus.recentEvents.length === 0 ? <div className="empty-state reference-empty"><p>暂无事件</p><small>OBSERVE 事件会在本机保留最近记录并写入 SQLite moderation_log。</small></div> : (
          <div className="qq-event-list">
            {qqStatus.recentEvents.slice().reverse().map((event, index) => (
              <div className="qq-event-row" key={`${event.messageId}-${event.createdAt}-${index}`}>
                <div><strong>{event.label ? labelText(event.label) : "无有效结果"}</strong><span>{event.groupId} · {formatReferenceDate(event.createdAt)}</span></div>
                <span>{event.decision} / {event.action} · 奶蛙 {percent(event.naiwaFrogScore)} · 失败 {event.failedImages}</span>
              </div>
            ))}
          </div>
        )}
      </section>
    </div>
  );

  const renderSettings = () => (
    <div className="settings-page">
      <div className="settings-heading"><p className="eyebrow">LOCAL CONFIGURATION</p><h1>设置</h1></div>
      <div className="settings-grid">
        <section className="settings-card">
          <h2>视觉引擎</h2>
          <dl>
            <div><dt>后端</dt><dd>{appInfo.visionBackend}</dd></div>
            <div><dt>状态</dt><dd data-status={appInfo.visionAvailable ? "ready" : "error"}>{appInfo.visionAvailable ? "可用" : "未就绪"}</dd></div>
            <div><dt>说明</dt><dd>{appInfo.visionMessage}</dd></div>
            <div><dt>网络</dt><dd>{appInfo.networkRequiredForClassification ? "识别需要网络" : "识别不需要网络"}</dd></div>
          </dl>
        </section>
        <section className="settings-card">
          <h2>参考库</h2>
          <dl>
            <div><dt>奶龙</dt><dd>{appInfo.nailongReferenceCount} / 10 张</dd></div>
            <div><dt>奶蛙</dt><dd>{appInfo.naiwaFrogReferenceCount} / 10 张</dd></div>
            <div><dt>版本</dt><dd>{appInfo.referenceSetVersion}</dd></div>
            <div><dt>策略</dt><dd>pHash 粗筛 + SIFT + Lowe Ratio + RANSAC</dd></div>
          </dl>
        </section>
      </div>
      <p className="settings-note">当前版本不包含数据集采集、人工标注、训练、模型导出或云端图片上传链路。</p>
    </div>
  );

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand"><div className="brand-icon">NLNF</div><div><strong>NLNF</strong><span>CLASSICAL VISION</span></div></div>
        <div className="phase-card"><span>当前阶段</span><strong>{appInfo.phase}</strong><div className="phase-track"><span /></div></div>
        <nav className="side-nav">
          {(["identify", "qq", "references", "settings"] as AppView[]).map((item) => (
            <button type="button" className={view === item ? "active" : ""} key={item} onClick={() => setView(item)}>
              <span className="nav-glyph">{item === "identify" ? "⌕" : item === "qq" ? "⌁" : item === "references" ? "▧" : "⚙"}</span>
              {viewTitle(item)}
              {item === "qq" && <span className="soon-tag">OFF</span>}
            </button>
          ))}
        </nav>
        <div className="sidebar-footer"><span className="offline-dot" />本地优先 · 不上传图片</div>
      </aside>
      <main className="main-content">
        <header className="topbar"><div><span>NLNF</span><span className="breadcrumb-separator">/</span><span className="breadcrumb-current">{viewTitle(view)}</span></div><span className="version-label">v{appInfo.appVersion}</span></header>
        {view === "identify" ? renderIdentify() : view === "references" ? renderReferences() : view === "qq" ? renderQq() : renderSettings()}
      </main>
    </div>
  );
}

export default App;
