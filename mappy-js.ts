import { map, nop } from "checkin:object";

const start = Date.now();
function a(x) {
  return x * 2;
}

function b(x) {
  return x + 2;
}
for (let i = 0; i < 10000000; i++) {
  const array = [i, i + 1, i + 2, i + 3, i + 4];
  const result = map(array, i % 2 === 0 ? a : b);
  nop(result);
}
const end = Date.now();
console.log(end - start);
