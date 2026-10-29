/**
 * Upstream loads its wasm grammars before extracting; the binary carries its
 * grammars compiled in, so loading is nothing to do.
 */
export async function initGrammars(): Promise<void> {}

export async function loadGrammarsForLanguages(_languages: readonly string[]): Promise<void> {}
