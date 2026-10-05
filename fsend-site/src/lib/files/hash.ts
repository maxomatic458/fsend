/** xxh3 of the first `length` bytes of `blob`, or null if it is shorter. */
export async function prefixHash(
  blob: Blob,
  length: number,
): Promise<string | null> {
  if (blob.size < length) return null;
  const { createXXHash3 } = await import("hash-wasm");
  const hasher = await createXXHash3();
  hasher.init();
  const reader = blob.slice(0, length).stream().getReader();
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    hasher.update(value);
  }
  return hasher.digest("hex");
}
