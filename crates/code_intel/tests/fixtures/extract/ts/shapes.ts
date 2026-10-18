import { readText } from "./io";
import type { Options } from "./options";

export interface Shape {
  area(): number;
  readonly name: string;
}

export abstract class Base implements Shape {
  abstract area(): number;
  constructor(public name: string) {}
  describe(prefix: string, digits = 2): string {
    return `${prefix} ${this.name}: ${this.area().toFixed(digits)}`;
  }
}

export class Circle extends Base {
  private radius: number;
  constructor(radius: number) {
    super("circle");
    this.radius = radius;
  }
  area(): number {
    return Math.PI * this.radius * this.radius;
  }
  static unit(): Circle {
    return new Circle(1);
  }
}

export function scale(shape: Shape, by: number): number;
export function scale(shapes: Shape[], by: number): number[];
export function scale(input: Shape | Shape[], by: number): number | number[] {
  const one = (s: Shape) => s.area() * by;
  return Array.isArray(input) ? input.map(one) : one(input);
}

function helper(values: Map<string, number>, fallback: number): number {
  const total = sum([...values.values()]);
  return total > 0 ? total : fallback;
}

export const sum = (values: number[]): number => values.reduce((a, b) => a + b, 0);

export async function load(path: string, options?: Options): Promise<Shape[]> {
  const text = await readText(path);
  const radii = JSON.parse(text) as number[];
  helper(new Map(), options?.fallback ?? 0);
  return radii.map((r) => new Circle(r));
}

export namespace Geometry {
  export function perimeter(shape: Circle): number {
    return 2 * Math.PI * shape.area();
  }
}

type Pair<A, B> = { first: A; second: B };

enum Unit {
  Metre,
  Inch,
}

export const DEFAULT_UNIT = Unit.Metre;

export function pair<A, B>(first: A, second: B): Pair<A, B> {
  return { first, second };
}
