// Copyright 2018-2025 the Deno authors. MIT license.
import { thingIsString } from "checkin:object";
const start = performance.now();

for (let i = 0; i < 1000000000; i++) {
  thingIsString(i);
}

const end = performance.now();
console.log(end - start);
