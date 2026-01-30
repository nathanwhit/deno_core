// Copyright 2018-2025 the Deno authors. MIT license.
import { Socket, Stream } from "checkin:object";
const obj = new Socket();
console.log(obj instanceof Stream);
