export type AppView = "identify" | "qq" | "references" | "settings";

export type ReferenceClass = "NAILONG" | "NAIWA_FROG";
export type ClassificationLabel = "NAILONG" | "NAIWA_FROG" | "OTHER" | "UNKNOWN";
export type ConfidenceLevel = "NONE" | "LOW" | "MEDIUM" | "HIGH" | "VERY_HIGH";
export type QqMode = "OFF" | "OBSERVE" | "AUTO_RECALL";

export interface VisionThresholds {
  matchThreshold: number;
  otherThreshold: number;
  recallThreshold: number;
  minMargin: number;
  minRecallMargin: number;
  minRecallInliers: number;
  minRecallRatio: number;
  minRecallCoverage: number;
  maxRecallReprojectionError: number;
}

export interface AppSettings {
  thresholds: VisionThresholds;
  developerMode: boolean;
}

export interface AppInfo {
  productName: string;
  appVersion: string;
  phase: string;
  visionBackend: string;
  visionAvailable: boolean;
  visionMessage: string;
  referenceSetVersion: number;
  nailongReferenceCount: number;
  naiwaFrogReferenceCount: number;
  networkRequiredForClassification: boolean;
}

export interface ImageInspection {
  bytes: number;
  sha256: string;
  format: "PNG" | "JPEG" | "WEBP" | "GIF";
  width: number;
  height: number;
  animated: boolean;
  frameCount: number;
}

export interface DecodeSummary {
  format: ImageInspection["format"];
  sourceFrameCount: number;
  sampledFrameIndices: number[];
  frameShape: [number, number, number, number];
}

export interface MatchResult {
  referenceId: string;
  class: ReferenceClass;
  keypointCount: number;
  goodMatchCount: number;
  inlierCount: number;
  inlierRatio: number;
  coverage: number;
  reprojectionError: number;
  phashDistance: number | null;
  score: number;
}

export interface ClassificationResult {
  label: ClassificationLabel;
  nailongScore: number;
  naiwaFrogScore: number;
  bestNailongReference: string | null;
  bestNaiwaReference: string | null;
  bestMatch: MatchResult | null;
  inlierCount: number;
  inlierRatio: number;
  coverage: number;
  reprojectionError: number;
  confidenceLevel: ConfidenceLevel;
  geometryValid: boolean;
  sampledFrameCount: number;
  qualifyingNaiwaFrameCount: number;
}

export interface ReferenceInfo {
  id: string;
  class: ReferenceClass;
  filePath: string;
  sha256: string;
  phash: string;
  descriptorPath: string | null;
  width: number;
  height: number;
  createdAt: string;
  previewUrl?: string;
}

export interface QueuedImage {
  id: string;
  file: File;
  previewUrl: string;
  inspection: ImageInspection;
  classification?: ClassificationResult;
  classificationError?: string;
  classifying?: boolean;
  referenceAddingClass?: ReferenceClass;
  referenceAddedClasses?: ReferenceClass[];
  referenceError?: string;
  debugPreviewUrl?: string;
  debugLoading?: boolean;
  debugError?: string;
}

export interface StorageInfo {
  path: string;
  schemaVersion: number;
}

export interface QqGroupView {
  groupId: string;
  groupName: string;
  mode: QqMode;
  recallThreshold: number;
}

export interface QqEventView {
  groupId: string;
  messageId: string;
  label: ClassificationLabel | null;
  nailongScore: number;
  naiwaFrogScore: number;
  decision: string;
  action: string;
  classifiedImages: number;
  failedImages: number;
  createdAt: string;
}

export interface QqStatus {
  connected: boolean;
  actionEndpoint: string | null;
  eventEndpoint: string | null;
  tokenConfigured: boolean;
  autoRecallAvailable: boolean;
  groups: QqGroupView[];
  recentEvents: QqEventView[];
  lastError: string | null;
}

export const DISPLAY_NAMES: Record<ClassificationLabel, string> = {
  NAILONG: "奶龙",
  NAIWA_FROG: "奶蛙",
  OTHER: "其他",
  UNKNOWN: "无法确定",
};

export const REFERENCE_NAMES: Record<ReferenceClass, string> = {
  NAILONG: "奶龙",
  NAIWA_FROG: "奶蛙",
};
