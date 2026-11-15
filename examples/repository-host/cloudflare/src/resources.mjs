// Payload bounds per active job. The codec's 6 MiB arena is independent of these.
export const MEMORY_LIMITS = Object.freeze({
  canonicalCacheBytes: 24 * 1024 * 1024, representationCacheBytes: 8 * 1024 * 1024,
  chunkCacheBytes: 16 * 1024 * 1024, cacheEntries: 8192,
  canonicalCaptureBytes: 8 * 1024 * 1024, representationCaptureBytes: 1024 * 1024,
  chunkBytes: 1024 * 1024, refPageRows: 128, metadataEntries: 256,
});
