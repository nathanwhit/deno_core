import { nextTick } from "ext:checkin_node/next_tick.ts";
import { op_exit } from "ext:core/ops";

export const process = {
  nextTick,
  exit: op_exit,
};

export default process;
