import { createBlankCell, createEmptyFrame, frameSize, frameToText } from "bootty.js/node";

const frame = createEmptyFrame({ cols: 24, rows: 4 });
frame.cells.push(
  ...Array.from("Bootty", (text, x) => createBlankCell(x, 0, { text })),
);

console.log(frameSize(frame));
console.log(frameToText(frame));
