import type { ContentPart } from '../generated/bindings';

/** A picture attached to a message: a reference to the workspace image store. */
export type ImagePart = Extract<ContentPart, { type: 'image' }>;

/** A message's pictures, in order. */
export const imageParts = (content: readonly ContentPart[]): ImagePart[] =>
  content.filter((part): part is ImagePart => part.type === 'image');
