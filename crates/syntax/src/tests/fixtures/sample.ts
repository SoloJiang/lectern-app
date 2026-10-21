// A synthetic TypeScript sample.
import { readFile } from "./io";

export interface Point {
  readonly x: number;
  y?: string;
}

const MAX_POINTS = 0x10;

export function scale(p: Point, by: number): Point {
  const label = `scaled ${by}x`;
  return { x: p.x * by, y: label };
}

class Grid<T> extends Base implements Shape {
  private cells: Array<T> = [];
  async load(path: string) {
    await readFile(path, /\.json$/);
  }
}
