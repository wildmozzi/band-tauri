-- =====================================================
-- BandSpace Subscriptions / Payments Migration
-- Version: 007
-- Description: 구독·결제 정보를 DB에 저장 (서버 메모리 Map 대체, P3-5)
--
-- 서버(server/src/repositories/subscriptionRepository.ts)가 사용한다.
-- 이 마이그레이션 전에는 결제 API가 503을 반환한다 (메모리 폴백 없음).
--
-- 보안:
--   - 사용자(authenticated)는 자기 행 SELECT만 가능
--   - INSERT/UPDATE/DELETE 및 함수 실행은 service_role(서버)만
-- 정합성:
--   - payments.payment_key / payments.order_id 유니크 → 멱등성 키
--   - 결제+구독 기록, 취소는 각각 함수 하나(=한 트랜잭션)로 처리
-- =====================================================

-- -----------------------------------------------------
-- 1. subscriptions
-- -----------------------------------------------------
CREATE TABLE IF NOT EXISTS public.subscriptions (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
  plan_type TEXT NOT NULL CHECK (plan_type IN ('standard', 'pro')),
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'cancelled', 'expired')),
  amount INTEGER NOT NULL CHECK (amount >= 0),
  payment_key TEXT,
  start_date TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  end_date TIMESTAMPTZ NOT NULL,
  cancelled_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  CHECK (end_date > start_date)
);

COMMENT ON TABLE public.subscriptions IS '유료 플랜 구독 (서버 service_role만 쓰기)';

CREATE INDEX IF NOT EXISTS idx_subscriptions_user_active
  ON public.subscriptions (user_id, created_at DESC) WHERE status = 'active';
CREATE INDEX IF NOT EXISTS idx_subscriptions_active_end
  ON public.subscriptions (end_date) WHERE status = 'active';

-- -----------------------------------------------------
-- 2. payments (토스 결제 기록)
--    결제 기록은 계정 삭제와 무관하게 보존한다 (FK 없이 user_id만 저장)
-- -----------------------------------------------------
CREATE TABLE IF NOT EXISTS public.payments (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL,
  order_id TEXT NOT NULL,
  payment_key TEXT NOT NULL,
  plan_type TEXT NOT NULL CHECK (plan_type IN ('standard', 'pro')),
  amount INTEGER NOT NULL CHECK (amount >= 0),
  status TEXT NOT NULL DEFAULT 'confirmed' CHECK (status IN ('confirmed', 'cancelled')),
  subscription_id UUID REFERENCES public.subscriptions(id) ON DELETE SET NULL,
  cancel_reason TEXT,
  cancelled_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  CONSTRAINT payments_payment_key_key UNIQUE (payment_key),
  CONSTRAINT payments_order_id_key UNIQUE (order_id)
);

COMMENT ON TABLE public.payments IS '토스페이먼츠 승인 기록. payment_key/order_id 유니크 = 멱등성 키';

CREATE INDEX IF NOT EXISTS idx_payments_user_id ON public.payments (user_id);
CREATE INDEX IF NOT EXISTS idx_payments_subscription_id ON public.payments (subscription_id);

-- -----------------------------------------------------
-- 3. RLS: 본인 행 SELECT만. 쓰기 정책 없음 → anon/authenticated는 쓰기 불가
--    (service_role은 RLS를 우회한다)
-- -----------------------------------------------------
ALTER TABLE public.subscriptions ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.payments ENABLE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS "subscriptions_select_own" ON public.subscriptions;
CREATE POLICY "subscriptions_select_own" ON public.subscriptions
  FOR SELECT TO authenticated
  USING (auth.uid() = user_id);

DROP POLICY IF EXISTS "payments_select_own" ON public.payments;
CREATE POLICY "payments_select_own" ON public.payments
  FOR SELECT TO authenticated
  USING (auth.uid() = user_id);

-- 테이블 권한도 최소화 (RLS와 이중 방어)
REVOKE ALL ON public.subscriptions FROM anon, authenticated;
REVOKE ALL ON public.payments FROM anon, authenticated;
GRANT SELECT ON public.subscriptions TO authenticated;
GRANT SELECT ON public.payments TO authenticated;
GRANT ALL ON public.subscriptions TO service_role;
GRANT ALL ON public.payments TO service_role;

-- -----------------------------------------------------
-- 4. record_confirmed_payment: 승인된 결제 + 구독을 한 트랜잭션으로 기록
--    - 같은 사용자·같은 paymentKey·orderId 재요청 → 기존 구독 반환 (멱등)
--    - 다른 사용자/다른 조합과 키가 겹치면 unique_violation(23505)
-- -----------------------------------------------------
CREATE OR REPLACE FUNCTION public.record_confirmed_payment(
  p_user_id UUID,
  p_order_id TEXT,
  p_payment_key TEXT,
  p_plan_type TEXT,
  p_amount INTEGER,
  p_period_days INTEGER
)
RETURNS public.subscriptions
LANGUAGE plpgsql
SET search_path = public
AS $$
DECLARE
  v_existing public.payments;
  v_sub public.subscriptions;
BEGIN
  SELECT * INTO v_existing
    FROM public.payments
   WHERE payment_key = p_payment_key OR order_id = p_order_id
   LIMIT 1
   FOR UPDATE;

  IF FOUND THEN
    IF v_existing.user_id = p_user_id
       AND v_existing.payment_key = p_payment_key
       AND v_existing.order_id = p_order_id
       AND v_existing.subscription_id IS NOT NULL THEN
      SELECT * INTO v_sub FROM public.subscriptions WHERE id = v_existing.subscription_id;
      IF FOUND THEN
        RETURN v_sub;
      END IF;
    END IF;
    RAISE EXCEPTION 'payment already recorded (order_id=%)', p_order_id
      USING ERRCODE = 'unique_violation';
  END IF;

  INSERT INTO public.subscriptions (user_id, plan_type, status, amount, payment_key, start_date, end_date)
  VALUES (p_user_id, p_plan_type, 'active', p_amount, p_payment_key, NOW(), NOW() + make_interval(days => p_period_days))
  RETURNING * INTO v_sub;

  -- 동시 요청은 여기서 유니크 제약에 걸려 전체 롤백된다 (구독 행도 남지 않음)
  INSERT INTO public.payments (user_id, order_id, payment_key, plan_type, amount, status, subscription_id)
  VALUES (p_user_id, p_order_id, p_payment_key, p_plan_type, p_amount, 'confirmed', v_sub.id);

  RETURN v_sub;
END;
$$;

-- -----------------------------------------------------
-- 5. cancel_subscription: 구독 취소 (+ 결제 취소 표시)를 한 트랜잭션으로
--    active가 아니면 false
-- -----------------------------------------------------
CREATE OR REPLACE FUNCTION public.cancel_subscription(
  p_subscription_id UUID,
  p_cancel_payment BOOLEAN,
  p_reason TEXT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
SET search_path = public
AS $$
BEGIN
  UPDATE public.subscriptions
     SET status = 'cancelled', cancelled_at = NOW(), updated_at = NOW()
   WHERE id = p_subscription_id AND status = 'active';

  IF NOT FOUND THEN
    RETURN FALSE;
  END IF;

  IF p_cancel_payment THEN
    UPDATE public.payments
       SET status = 'cancelled', cancel_reason = p_reason, cancelled_at = NOW(), updated_at = NOW()
     WHERE subscription_id = p_subscription_id AND status = 'confirmed';
  END IF;

  RETURN TRUE;
END;
$$;

-- 함수는 서버(service_role)만 실행
REVOKE ALL ON FUNCTION public.record_confirmed_payment(UUID, TEXT, TEXT, TEXT, INTEGER, INTEGER) FROM PUBLIC, anon, authenticated;
REVOKE ALL ON FUNCTION public.cancel_subscription(UUID, BOOLEAN, TEXT) FROM PUBLIC, anon, authenticated;
GRANT EXECUTE ON FUNCTION public.record_confirmed_payment(UUID, TEXT, TEXT, TEXT, INTEGER, INTEGER) TO service_role;
GRANT EXECUTE ON FUNCTION public.cancel_subscription(UUID, BOOLEAN, TEXT) TO service_role;

-- PostgREST 스키마 캐시 갱신 (새 테이블/함수를 바로 인식)
NOTIFY pgrst, 'reload schema';

DO $$
BEGIN
  RAISE NOTICE '007_subscriptions.sql 마이그레이션 완료!';
END $$;
