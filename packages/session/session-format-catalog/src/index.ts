/** Build-static first-party Session format migration catalog. */

export { sessionFormatCatalog } from './generated.ts'
// Declarations only: released vocabulary no plugin writes, kept so logs that carry it still open.
export type * from './retired-vocabulary.ts'
export { SessionFormatUnsupportedMigrationError } from 'bake-session-format'
