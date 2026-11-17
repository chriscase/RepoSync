import type { ReactNode } from 'react';
import type { VocabularyId } from './layoutCatalog';
import { VocabularyContext } from './vocabulary';

/** Provides the current layout's vocabulary to the routed page (#264 E3). */
export function VocabularyProvider({ vocabulary, children }: { readonly vocabulary: VocabularyId; readonly children: ReactNode }) {
  return <VocabularyContext.Provider value={vocabulary}>{children}</VocabularyContext.Provider>;
}
