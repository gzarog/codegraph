/**
 * Upstream's native-kernel route. The binary has one extractor (upstream's
 * kernel, with the pre-parse passes and the macro-method pass upstream runs
 * around it), so a "kernel" extraction is the binary's extraction.
 */
import { extractFromSource, type ExtractionResult } from '../index';

export function tryKernelExtract(filePath: string, source: string, language: string): ExtractionResult | null {
  return extractFromSource(filePath, source, language);
}

export function resetKernelForTests(): void {}
