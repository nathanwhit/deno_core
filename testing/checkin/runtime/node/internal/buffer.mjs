export class Buffer extends Uint8Array {
  constructor(...args) {
    super(...args);
  }

  static from(value, encoding) {
    if (typeof value === "string") {
      if (encoding && encoding !== "utf8" && encoding !== "utf-8") {
        throw new TypeError(`Unsupported encoding: ${encoding}`);
      }
      return new Buffer(new TextEncoder().encode(value));
    }
    if (ArrayBuffer.isView(value)) {
      return new Buffer(
        value.buffer.slice(
          value.byteOffset,
          value.byteOffset + value.byteLength,
        ),
      );
    }
    if (value instanceof ArrayBuffer) {
      return new Buffer(value);
    }
    if (Array.isArray(value)) {
      return new Buffer(Uint8Array.from(value));
    }
    throw new TypeError("Unsupported Buffer.from input");
  }

  static isEncoding(encoding) {
    if (typeof encoding !== "string") return false;
    const enc = encoding.toLowerCase();
    switch (enc) {
      case "utf8":
      case "utf-8":
      case "ascii":
      case "latin1":
      case "binary":
      case "hex":
      case "base64":
      case "base64url":
      case "ucs2":
      case "ucs-2":
      case "utf16le":
      case "utf-16le":
        return true;
      default:
        return false;
    }
  }

  static byteLength(value, encoding) {
    if (typeof value === "string") {
      if (encoding && !Buffer.isEncoding(encoding)) {
        throw new TypeError(`Unsupported encoding: ${encoding}`);
      }
      return new TextEncoder().encode(value).length;
    }
    if (value instanceof ArrayBuffer) {
      return value.byteLength;
    }
    if (ArrayBuffer.isView(value)) {
      return value.byteLength;
    }
    if (value instanceof Buffer) {
      return value.length;
    }
    throw new TypeError("Unsupported Buffer.byteLength input");
  }

  toString(encoding, start, end) {
    if (encoding === undefined) {
      encoding = "utf8";
    } else if (typeof encoding !== "string") {
      end = start;
      start = encoding;
      encoding = "utf8";
    }

    if (!Buffer.isEncoding(encoding)) {
      throw new TypeError(`Unsupported encoding: ${encoding}`);
    }

    const len = this.length >>> 0;
    const startIndex = Math.min(
      len,
      Math.max(0, Number(start ?? 0) | 0),
    );
    const endIndex = Math.min(
      len,
      Math.max(startIndex, Number(end ?? len) | 0),
    );
    if (startIndex === 0 && endIndex === len) {
      return new TextDecoder().decode(this);
    }
    return new TextDecoder().decode(this.subarray(startIndex, endIndex));
  }
}

export default Buffer;
