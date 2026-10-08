export function staticValueSpecifiers(code: string, file?: string): string[];
export function builtHeavyHits(buildDirectory: string, startupFiles: string[]): {
  chunk: string;
  module: string;
}[];
export function traceBuiltEager(buildDirectory: string, htmlFile?: string): {
  files: string[];
  rawBytes: number;
  gzipBytes: number;
  assets: Map<string, {
    path: string;
    bytes: number;
    gzipBytes: number;
    dependencies: string[];
  }>;
};
