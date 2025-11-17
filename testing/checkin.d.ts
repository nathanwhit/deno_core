// deno-lint-ignore-file no-explicit-any
// Copyright 2018-2025 the Deno authors. MIT license.

/// <reference lib="dom" />

export type * from "../core/core.d.ts";

declare global {
  // Types and method unavailable in TypeScript by default.
  interface PromiseConstructor {
    withResolvers<T>(): {
      promise: Promise<T>;
      resolve: (value: T | PromiseLike<T>) => void;
      reject: (reason?: any) => void;
    };
  }

  interface ArrayBuffer {
    transfer(size: number): ArrayBuffer;
  }

  interface SharedArrayBuffer {
    transfer(size: number): SharedArrayBuffer;
  }

  namespace Deno {
    export function refTimer(id: number): void;
    export function unrefTimer(id: number): void;
  }
}

declare module "checkin:object" {
  export function nop(): void;
  export function rustObjectAssign(target: any, source: any): any;
  export function map(array: any[], func: (value: any) => any): any[];
  export function map2(array: any[], func: (value: any) => any): any[];
  export function thingIsString(thing: any): boolean;
  export function testObjectWrap(): any;
  export function testEnumWrap(): any;
}