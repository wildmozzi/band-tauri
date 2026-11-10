-- =====================================================
-- BandSpace Payment Orders Migration
-- Version: 009 (007_subscriptions.sql 이후 실행)
-- Description: 결제 주문(orderId)을 서버가 발급하고 사용자에 묶는다 (P3-5b)
--
-- 서버(server/src/repositories/paymentOrderRepository.ts)가 사용한다.
-- 이 마이그레이션 전에는 POST /api/payment/orders 와 confirm이 503을 반환한다.
--
-- 흐름:
--   1) POST /api/payment/orders → 서버가 order_id를 만들고 (사용자, 플랜, 서버 가격표 금액, 만료 30분) 저장
--   2) 클라이언트는 그 order_id로 토스 결제창을 연다
--   3) confirm 시 order_id가 그 사용자 소유이고 플랜·금액이 일치해야만 토스 승인으로 진행
--      (클라이언트가 만든 order_id는 이 테이블에 없으므로 거부된다)
--   4) confirm 시도 시각(confirm_attempted_at)을 남긴다 → payments 행이 없어졌으면 '실패'로 보고한다
--
-- 보안:
--   - 사용자(authenticated)는 자기 주문 SELECT만 가능
--   - INSERT/UPDATE/DELETE는 service_role(서버)만
-- =====================================================

CREATE TABLE IF NOT EXISTS public.payment_orders (
  -- 토스 orderId 규칙: 영문·숫자·-·_ 6~64자
  order_id TEXT PRIMARY KEY CHECK (order_id ~ '^[A-Za-z0-9_-]{6,64}$'),
  user_id UUID NOT NULL,
  plan_type TEXT NOT NULL CHECK (plan_type IN ('standard', 'pro')),
  amount INTEGER NOT NULL CHECK (amount > 0),
  expires_at TIMESTAMPTZ NOT NULL,
  confirm_attempted_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  CHECK (expires_at > created_at)
);

COMMENT ON TABLE public.payment_orders IS '서버 발급 결제 주문 (orderId ↔ 사용자·플랜·금액 바인딩, 서버 service_role만 쓰기)';

CREATE INDEX IF NOT EXISTS idx_payment_orders_user ON public.payment_orders (user_id, created_at DESC);

ALTER TABLE public.payment_orders ENABLE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS "payment_orders_select_own" ON public.payment_orders;
CREATE POLICY "payment_orders_select_own" ON public.payment_orders
  FOR SELECT TO authenticated
  USING ((SELECT auth.uid()) = user_id);

REVOKE ALL ON public.payment_orders FROM anon, authenticated;
GRANT SELECT ON public.payment_orders TO authenticated;
GRANT ALL ON public.payment_orders TO service_role;

NOTIFY pgrst, 'reload schema';

DO $$
BEGIN
  RAISE NOTICE '009_payment_orders.sql 마이그레이션 완료!';
END $$;
