import { createContext, useContext } from 'react';
import type { VocabularyId } from './layoutCatalog';
import { TEXT_CATALOGUE, type TextKey } from './textCatalogue';

/**
 * #264 E3: the routed page reads its wording from the vocabulary the current layout
 * names. Changing layout re-renders text; it never remounts the page, so drafts,
 * dialogs, focus and pending requests are kept. Outside the shell (sign-in, tests
 * that render a page alone) the existing vocabulary applies.
 */
export const VocabularyContext = createContext<VocabularyId>('existing');

export function useVocabulary(): VocabularyId {
  return useContext(VocabularyContext);
}

/** Returns the catalogue text for a key in the current vocabulary. */
export function useText(): (key: TextKey) => string {
  const vocabulary = useVocabulary();
  return key => TEXT_CATALOGUE[key][vocabulary];
}
