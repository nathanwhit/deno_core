// Copyright 2018-2025 the Deno authors. MIT license.
import { op_read_file_text_sync, op_path_to_url, op_cwd } from "ext:core/ops";
import { core } from "ext:core/mod.js";

const moduleCache = new Map<string, Module>();
const builtinModules = new Map<string, unknown>();

class Module {
  exports: any = {};
  loaded = false;

  constructor(public id: string, public filename: string) {}

  get dirname(): string {
    return this.filename.substring(0, this.filename.lastIndexOf("/"));
  }

  static wrapper = [
    "(function (exports, require, module, __filename, __dirname) { ",
    "\n});",
  ];

  static wrap(script: string): string {
    // Remove shebang if present
    script = script.replace(/^#!.*?\n/, "");
    return `${Module.wrapper[0]}${script}${Module.wrapper[1]}`;
  }
}

function getCallerDir(): string {
  // Get the directory of the calling file by parsing the stack trace
  const oldPrepareStackTrace = Error.prepareStackTrace;
  const oldStackTraceLimit = Error.stackTraceLimit;
  Error.stackTraceLimit = 10;
  Error.prepareStackTrace = (_err, stack) => stack;
  const err = new Error();
  const stack = err.stack as unknown as NodeJS.CallSite[];
  Error.prepareStackTrace = oldPrepareStackTrace;
  Error.stackTraceLimit = oldStackTraceLimit;

  // Find the first frame that's not in our module system
  for (const frame of stack) {
    const filename = frame.getFileName();
    if (filename && !filename.includes("ext:checkin_node/module.ts")) {
      // Convert file:// URL to path if needed
      let path = filename;
      if (path.startsWith("file://")) {
        path = path.slice(7);
      }
      // Get directory part
      const lastSlash = path.lastIndexOf("/");
      return lastSlash >= 0 ? path.substring(0, lastSlash) : ".";
    }
  }
  return op_cwd();
}

function resolveFilename(request: string, parent: Module | null): string {
  // 1. Handle "node:" builtins
  if (request.startsWith("node:") || builtinModules.has(request)) {
    return request.startsWith("node:") ? request : `node:${request}`;
  }

  // 2. Handle relative paths
  if (request.startsWith("./") || request.startsWith("../")) {
    const baseDir = parent ? parent.dirname : getCallerDir();
    return tryExtensions(resolvePath(baseDir, request));
  }

  // 3. Handle absolute paths
  if (request.startsWith("/")) {
    return tryExtensions(request);
  }

  throw new Error(`Cannot find module '${request}'`);
}

function resolvePath(base: string, relative: string): string {
  // Simple path resolution for ../ and ./
  const parts = base.split("/").filter((p) => p);
  for (const part of relative.split("/")) {
    if (part === "..") parts.pop();
    else if (part !== ".") parts.push(part);
  }
  return "/" + parts.join("/");
}

function tryExtensions(path: string): string {
  for (const ext of ["", ".js", ".json", "/index.js"]) {
    try {
      op_read_file_text_sync(path + ext);
      return path + ext;
    } catch {
      /* continue */
    }
  }
  return path;
}

function loadModule(module: Module): void {
  const content = op_read_file_text_sync(module.filename);
  const wrapped = Module.wrap(content);
  const fileUrl = op_path_to_url(module.filename);

  // evalContext returns [result, error]
  // hostDefinedOptions[0] = true means NOT a module (CJS script)
  const [fn, error] = core.evalContext(wrapped, fileUrl, [true]);
  if (error) {
    throw error.thrown ?? new Error(
      `Error loading module ${module.filename}: compile error`
    );
  }

  const moduleRequire = makeRequire(module);
  fn(module.exports, moduleRequire, module, module.filename, module.dirname);
  module.loaded = true;
}

function makeRequire(parent: Module | null) {
  const require = (request: string): unknown => {
    const filename = resolveFilename(request, parent);

    // Builtin modules
    if (filename.startsWith("node:")) {
      const name = filename.slice(5);
      return builtinModules.get(name) ?? builtinModules.get(filename);
    }

    // Check cache (handles circular deps by returning partial exports)
    if (moduleCache.has(filename)) {
      return moduleCache.get(filename)!.exports;
    }

    // Create module, cache BEFORE loading (for circular deps)
    const module = new Module(request, filename);
    moduleCache.set(filename, module);
    loadModule(module);
    return module.exports;
  };

  require.cache = moduleCache;
  require.resolve = (r: string) => resolveFilename(r, parent);
  return require;
}

export function registerBuiltin(name: string, exports: unknown): void {
  builtinModules.set(name, exports);
  builtinModules.set(`node:${name}`, exports);
}

export const require = makeRequire(null);
export { Module };
