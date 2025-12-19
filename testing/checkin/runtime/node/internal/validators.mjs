import { ERR_INVALID_ARG_TYPE } from "ext:checkin_node/internal/errors.ts";
export function validateAbortSignal(signal, name) {
  if (
    typeof signal !== "object" ||
    !("aborted" in signal)
  ) {
    throw new ERR_INVALID_ARG_TYPE(name, "AbortSignal", signal);
  }
}

export function validateFunction(value, name) {
  if (typeof value !== "function") {
    throw new ERR_INVALID_ARG_TYPE(name, "Function", value);
  }
}

export function validateObject(value, name) {
  if (typeof value !== "object" || value === null) {
    throw new ERR_INVALID_ARG_TYPE(name, "Object", value);
  }
}

export function validateBoolean(value, name) {
  if (typeof value !== "boolean") {
    throw new ERR_INVALID_ARG_TYPE(name, "Boolean", value);
  }
}

export function validateInteger(value, name) {
  if (typeof value !== "number" || !Number.isInteger(value)) {
    throw new ERR_INVALID_ARG_TYPE(name, "Integer", value);
  }
}

export function validateNumber(value, name) {
  if (typeof value !== "number") {
    throw new ERR_INVALID_ARG_TYPE(name, "Number", value);
  }
}
