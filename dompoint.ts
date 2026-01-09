import { DOMPoint, DOMPointReadOnly } from "checkin:object";

const p1 = new DOMPoint(100, 200, 300, 400);
console.log("p1 created");
console.log("p1.x =", p1.x);
console.log("p1.y =", p1.y);
console.log("p1.z =", p1.z);
console.log("p1.w =", p1.w);

const p2 = new DOMPointReadOnly(10, 20, 30, 40);
console.log("p2 created");
console.log("p2.x =", p2.x);
console.log("p2.y =", p2.y);

console.log("p1 instanceof DOMPoint =", p1 instanceof DOMPoint);
console.log("p1 instanceof DOMPointReadOnly =", p1 instanceof DOMPointReadOnly);
console.log("p2 instanceof DOMPoint =", p2 instanceof DOMPoint);
console.log("p2 instanceof DOMPointReadOnly =", p2 instanceof DOMPointReadOnly);

const points: DOMPoint[] = [];

const start = Date.now();
for (let i = 0; i < 10000000; i++) {
  points.push(new DOMPoint(i, i, i, i));
  points[i].x = i;
  points[i].y = points[i].x;
  points[i].z = points[i].y;
  points[i].w = points[i].z;
}

console.log("Time taken:", Date.now() - start);
