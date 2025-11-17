import { thingIsStringRust } from "checkin:object";
await new Promise((resolve) => setTimeout(resolve, 10000));
const start = Date.now();

for (let i = 0; i < 1000000000; i++) {
  thingIsStringRust(i);
}

const end = Date.now();
console.log(end - start);

setInterval(() => {
  console.log(end - start);
}, 1000);
