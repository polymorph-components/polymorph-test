/** @module Interface polymorph:test/test-context@0.1.0 **/

export class Context {
  /**
   * This type does not have a public constructor.
   */
  private constructor();
  diagnostic(msg: string): Promise<void>;
}
