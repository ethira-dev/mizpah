import { useCallback, useSyncExternalStore } from "react"

import {
  findSavedByExpression as findSavedByExpressionStorage,
  getQueryLibrarySnapshot,
  pushHistory as pushHistoryStorage,
  removeHistory as removeHistoryStorage,
  removeSaved as removeSavedStorage,
  removeSavedByExpression as removeSavedByExpressionStorage,
  saveQuery as saveQueryStorage,
  subscribeQueryLibrary,
  type QueryHistoryEntry,
  type SavedQuery,
} from "@/lib/query-library-storage"

export type QueryLibrary = {
  history: QueryHistoryEntry[]
  saved: SavedQuery[]
  pushHistory: (expression: string) => void
  removeHistory: (expression: string) => void
  saveQuery: (input: { expression: string; name?: string }) => SavedQuery | null
  removeSaved: (id: string) => void
  removeSavedByExpression: (expression: string) => void
  findSavedByExpression: (expression: string) => SavedQuery | undefined
}

export function useQueryLibrary(): QueryLibrary {
  const snapshot = useSyncExternalStore(
    subscribeQueryLibrary,
    getQueryLibrarySnapshot,
    getQueryLibrarySnapshot
  )

  const pushHistory = useCallback((expression: string) => {
    pushHistoryStorage(expression)
  }, [])

  const removeHistory = useCallback((expression: string) => {
    removeHistoryStorage(expression)
  }, [])

  const saveQuery = useCallback(
    (input: { expression: string; name?: string }) => {
      return saveQueryStorage(input)
    },
    []
  )

  const removeSaved = useCallback((id: string) => {
    removeSavedStorage(id)
  }, [])

  const removeSavedByExpression = useCallback((expression: string) => {
    removeSavedByExpressionStorage(expression)
  }, [])

  const findSavedByExpression = useCallback((expression: string) => {
    return findSavedByExpressionStorage(expression)
  }, [])

  return {
    history: snapshot.history,
    saved: snapshot.saved,
    pushHistory,
    removeHistory,
    saveQuery,
    removeSaved,
    removeSavedByExpression,
    findSavedByExpression,
  }
}
