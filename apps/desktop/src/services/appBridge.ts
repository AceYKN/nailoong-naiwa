import { invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  ClassificationResult,
  DecodeSummary,
  ImageInspection,
  ReferenceClass,
  ReferenceInfo,
  QqMode,
  QqStatus,
  StorageInfo,
} from "../types/domain";

export async function getAppInfo(): Promise<AppInfo> {
  return invoke<AppInfo>("app_info");
}

export async function inspectImage(bytes: Uint8Array): Promise<ImageInspection> {
  return invoke<ImageInspection>("inspect_image", { bytes: Array.from(bytes) });
}

export async function decodeImageSummary(bytes: Uint8Array, maxSampleFrames = 12): Promise<DecodeSummary> {
  return invoke<DecodeSummary>("decode_image_summary", {
    bytes: Array.from(bytes),
    maxSampleFrames,
  });
}

export async function classifyImage(bytes: Uint8Array, maxSampleFrames = 12): Promise<ClassificationResult> {
  return invoke<ClassificationResult>("classify_image", {
    bytes: Array.from(bytes),
    maxSampleFrames,
  });
}

export async function debugMatchImage(bytes: Uint8Array, referenceId: string): Promise<Uint8Array> {
  const encoded = await invoke<number[]>("debug_match_image", {
    bytes: Array.from(bytes),
    referenceId,
  });
  return new Uint8Array(encoded);
}

export async function listReferences(): Promise<ReferenceInfo[]> {
  return invoke<ReferenceInfo[]>("list_references");
}

export async function readReference(id: string): Promise<Uint8Array> {
  const bytes = await invoke<number[]>("read_reference", { id });
  return new Uint8Array(bytes);
}

export async function addReference(referenceClass: ReferenceClass, bytes: Uint8Array): Promise<ReferenceInfo> {
  return invoke<ReferenceInfo>("add_reference", {
    class: referenceClass,
    bytes: Array.from(bytes),
  });
}

export async function removeReference(id: string): Promise<void> {
  await invoke("remove_reference", { id });
}

export async function initializeStorage(): Promise<StorageInfo> {
  return invoke<StorageInfo>("initialize_storage");
}

export async function getQqStatus(): Promise<QqStatus> {
  return invoke<QqStatus>("qq_status");
}

export async function connectQq(actionEndpoint: string, eventEndpoint: string, token: string): Promise<QqStatus> {
  return invoke<QqStatus>("connect_qq", {
    actionEndpoint,
    eventEndpoint,
    token: token.trim() || null,
  });
}

export async function disconnectQq(): Promise<QqStatus> {
  return invoke<QqStatus>("disconnect_qq");
}

export async function setQqGroupMode(
  groupId: string,
  groupName: string,
  mode: QqMode,
  recallThreshold = 0.98,
): Promise<QqStatus> {
  return invoke<QqStatus>("set_qq_group_mode", {
    groupId,
    groupName,
    mode,
    recallThreshold,
  });
}
