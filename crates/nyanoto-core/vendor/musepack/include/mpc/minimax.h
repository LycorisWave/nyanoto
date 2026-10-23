/* Replacement for minimax.h from the Musepack sources (that file is LGPL):
 * libmpcdec only uses these two trivial macros. Written for nyanoto. */
#pragma once

#define mini(A, B) ((A) < (B) ? (A) : (B))
#define absi(A) ((A) >= 0 ? (A) : -(A))
