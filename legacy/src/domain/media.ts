/** Media extensions describe presentation; Rust validates the bytes before assigning a MIME. */
export function mediaKind(path: string): "image" | "video" | "svg" | null {
  const extension = path.split(".").at(-1)?.toLowerCase();
  if (extension === "svg") return "svg";
  if (["png", "jpg", "jpeg", "gif", "webp", "avif", "ico", "bmp"].includes(extension ?? "")) return "image";
  if (["mp4", "webm", "mov", "ogv"].includes(extension ?? "")) return "video";
  return null;
}

/** The largest slice a comparison ever asks for. */
const MEDIA_COMPARE_CHUNK_BYTES = 1024 * 1024;

/**
 * Whether two media payloads are the same bytes, compared in bounded chunks so neither is held
 * whole to answer the question. The answer is the bytes themselves: anything cheaper could call a
 * changed file unchanged, and a refresh that trusted it would keep showing the old media.
 *
 * `stillWanted` is asked at every chunk, so a comparison the pane has moved on from stops there.
 * That answer is `null`, which is not `false`.
 */
export async function sameMediaBytes(
  left: Blob,
  right: Blob,
  stillWanted: () => boolean = () => true,
): Promise<boolean | null> {
  if (left.type !== right.type || left.size !== right.size) return false;
  for (let offset = 0; offset < left.size; offset += MEDIA_COMPARE_CHUNK_BYTES) {
    if (!stillWanted()) return null;
    const end = Math.min(offset + MEDIA_COMPARE_CHUNK_BYTES, left.size);
    const leftChunk = new Uint8Array(await left.slice(offset, end).arrayBuffer());
    const rightChunk = new Uint8Array(await right.slice(offset, end).arrayBuffer());
    if (!leftChunk.every((byte, index) => byte === rightChunk[index])) return false;
  }
  return true;
}

export function svgBlob(source: string): Blob {
  const xml = new DOMParser().parseFromString(source, "image/svg+xml");
  const root = xml.documentElement;
  if (
    xml.querySelector("parsererror") ||
    root.localName !== "svg" ||
    root.namespaceURI !== "http://www.w3.org/2000/svg"
  ) {
    throw new Error("This file is not a valid SVG document.");
  }
  return new Blob([source], { type: "image/svg+xml" });
}
