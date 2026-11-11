import { matchRoutes } from 'react-router-dom'
import { SERVICE_NAME } from './brand'
import { NOT_FOUND_TITLE, ROUTE_TABLE } from '../routeTable'

export { NOT_FOUND_TITLE }

/** 탭 제목: 홈은 'IndigoMuse — 온라인 합주실', 그 밖은 'IndigoMuse — <화면>'.
 *  라우트 표(routeTable.ts)를 라우터와 같은 규칙(matchRoutes)으로 매칭한다 */
export function pageTitleFor(pathname: string): string {
  const matches = matchRoutes([...ROUTE_TABLE], pathname)
  const hit = matches?.[matches.length - 1]?.route
  const title = hit ? hit.title : NOT_FOUND_TITLE
  return `${SERVICE_NAME} — ${title ?? '온라인 합주실'}`
}
