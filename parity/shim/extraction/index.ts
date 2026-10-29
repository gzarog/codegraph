/**
 * Upstream's `src/extraction` entry, as far as its tests use it: one file's
 * extraction, run by the binary under test (`codegraph __extract`).
 */
import { execFileSync } from 'node:child_process';
import { binary, binaryEnv } from '../binary';
import type { Edge, Node, UnresolvedReference } from '../index';

export interface ExtractionError {
  message: string;
  filePath?: string;
  line?: number;
  column?: number;
  severity: 'error' | 'warning';
  code?: string;
}

export interface ExtractionResult {
  nodes: Node[];
  edges: Edge[];
  unresolvedReferences: UnresolvedReference[];
  errors: ExtractionError[];
  durationMs: number;
}

/** Extract `source` as the file `filePath` (in `language`, else the one its path and content say). */
export function extractFromSource(filePath: string, source: string, language?: string): ExtractionResult {
  const args = ['__extract', filePath, ...(language ? ['--language', language] : [])];
  const out = execFileSync(binary(), args, { input: source, env: binaryEnv(), maxBuffer: 1 << 30 });
  return JSON.parse(out.toString('utf8')) as ExtractionResult;
}
