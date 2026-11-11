/** 클래스 이름 합치기: 거짓 값은 버린다 */
export function cx(...parts: Array<string | false | null | undefined>): string {
  return parts.filter(Boolean).join(' ')
}
