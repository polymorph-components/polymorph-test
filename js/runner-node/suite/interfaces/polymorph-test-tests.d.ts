/** @module Interface polymorph:test/tests@0.1.0 **/
export function all(): Promise<Array<TestCase>>;
export type Context = import('./polymorph-test-test-context.js').Context;
export type Outcome = OutcomeFailed | OutcomeSkipped;
export interface OutcomeFailed {
  tag: 'failed',
  val: string,
}
export interface OutcomeSkipped {
  tag: 'skipped',
  val: string,
}

export class TestCase implements Disposable {
  /**
   * This type does not have a public constructor.
   */
  private constructor();
  name(): string;
  run(ctx: Context): Promise<void>;
  [Symbol.dispose](): void;
}
