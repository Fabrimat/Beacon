/**
 * A world.
 *
 * Second paragraph, not in the doc.
 */
export class World {
    /** Gets a dimension. */
    getDimension(id: string): Dimension;
    getDimension(id: number): Dimension;
    readonly name: string;
    private constructor();
}
export class Dimension {}
export enum GameMode {
    Survival = "survival",
    Creative = "creative",
}
export interface Vec3 {
    x: number;
    y: number;
    z: number;
}
/**
 * @deprecated Use World.
 */
export function oldThing(a: number): void;
export const world: World;
export type Id = string | number;
