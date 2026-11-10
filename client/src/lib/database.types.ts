/**
 * Supabase 스키마 타입 (`supabase gen types typescript` 출력 형식과 호환).
 *
 * 근거: docs/migrations/001~009.sql. 운영 DB에 접속하지 않고 SQL 파일로 수작업 작성했다.
 * 스키마를 바꾸는 마이그레이션을 추가하면 이 파일도 같은 PR에서 갱신한다.
 *
 * 주의 — 마이그레이션 파일에 CREATE가 없는 부분은 코드 사용처(client/server)로 추정했다:
 *  - profiles, rooms, post_likes 테이블 전체 (001 이전에 대시보드에서 생성됨)
 *  - posts.author_name / author_role / instrument 컬럼 (클라이언트가 insert·filter에 사용)
 * 운영 DB와 대조하려면 사람이 `supabase gen types typescript --project-id <id>`로 생성해 diff를 본다.
 *
 * 생성기와 다른 점(의도적):
 *  - rooms.instrument_slots는 JSONB지만 앱 계약 형태(InstrumentSlotJson[])로 좁혔다.
 *    interface 값은 Json 인덱스 시그니처에 대입되지 않아서, 수정 금지 파일(RoomDetail)이 그대로 컴파일되게 한다.
 */

export type Json =
  | string
  | number
  | boolean
  | null
  | { [key: string]: Json | undefined }
  | Json[]

/** rooms.instrument_slots 원소 (004_add_instrument_slots.sql) */
export type InstrumentSlotJson = { instrument: string; count: number }

export type Database = {
  // supabase-js가 PostgREST 버전별 기능을 고를 때 쓴다 (gen types 기본값)
  __InternalSupabase: {
    PostgrestVersion: '12'
  }
  public: {
    Tables: {
      profiles: {
        Row: {
          id: string
          email: string | null
          name: string | null
          nickname: string | null
          birth_date: string | null
          gender: string | null
          phone: string | null
          audio_settings: Json | null
          created_at: string | null
          updated_at: string | null
        }
        Insert: {
          id: string
          email?: string | null
          name?: string | null
          nickname?: string | null
          birth_date?: string | null
          gender?: string | null
          phone?: string | null
          audio_settings?: Json | null
          created_at?: string | null
          updated_at?: string | null
        }
        Update: {
          id?: string
          email?: string | null
          name?: string | null
          nickname?: string | null
          birth_date?: string | null
          gender?: string | null
          phone?: string | null
          audio_settings?: Json | null
          created_at?: string | null
          updated_at?: string | null
        }
        Relationships: []
      }
      rooms: {
        Row: {
          id: string
          title: string
          description: string | null
          host_id: string
          max_participants: number
          current_participants: number
          status: string
          genre: string | null
          tags: string[]
          free_join: boolean | null
          instrument_slots: InstrumentSlotJson[] | null
          is_private: boolean
          has_password: boolean
          created_at: string
          updated_at: string | null
        }
        Insert: {
          id?: string
          title: string
          description?: string | null
          host_id: string
          max_participants?: number
          current_participants?: number
          status?: string
          genre?: string | null
          tags?: string[]
          free_join?: boolean | null
          instrument_slots?: InstrumentSlotJson[] | null
          is_private?: boolean
          has_password?: boolean
          created_at?: string
          updated_at?: string | null
        }
        Update: {
          id?: string
          title?: string
          description?: string | null
          host_id?: string
          max_participants?: number
          current_participants?: number
          status?: string
          genre?: string | null
          tags?: string[]
          free_join?: boolean | null
          instrument_slots?: InstrumentSlotJson[] | null
          is_private?: boolean
          has_password?: boolean
          created_at?: string
          updated_at?: string | null
        }
        Relationships: [
          {
            foreignKeyName: 'rooms_host_id_fkey'
            columns: ['host_id']
            isOneToOne: false
            referencedRelation: 'profiles'
            referencedColumns: ['id']
          },
        ]
      }
      room_secrets: {
        Row: {
          room_id: string
          password_hash: string
          created_at: string
          updated_at: string
        }
        Insert: {
          room_id: string
          password_hash: string
          created_at?: string
          updated_at?: string
        }
        Update: {
          room_id?: string
          password_hash?: string
          created_at?: string
          updated_at?: string
        }
        Relationships: [
          {
            foreignKeyName: 'room_secrets_room_id_fkey'
            columns: ['room_id']
            isOneToOne: true
            referencedRelation: 'rooms'
            referencedColumns: ['id']
          },
        ]
      }
      posts: {
        Row: {
          id: string
          author_id: string
          author_name: string | null
          author_role: string | null
          title: string
          content: string
          category: string | null
          instrument: string | null
          tags: string[] | null
          images: string[] | null
          likes_count: number | null
          comments_count: number | null
          views_count: number | null
          created_at: string | null
          updated_at: string | null
        }
        Insert: {
          id?: string
          author_id: string
          author_name?: string | null
          author_role?: string | null
          title: string
          content: string
          category?: string | null
          instrument?: string | null
          tags?: string[] | null
          images?: string[] | null
          likes_count?: number | null
          comments_count?: number | null
          views_count?: number | null
          created_at?: string | null
          updated_at?: string | null
        }
        Update: {
          id?: string
          author_id?: string
          author_name?: string | null
          author_role?: string | null
          title?: string
          content?: string
          category?: string | null
          instrument?: string | null
          tags?: string[] | null
          images?: string[] | null
          likes_count?: number | null
          comments_count?: number | null
          views_count?: number | null
          created_at?: string | null
          updated_at?: string | null
        }
        Relationships: [
          {
            foreignKeyName: 'posts_author_id_fkey'
            columns: ['author_id']
            isOneToOne: false
            referencedRelation: 'profiles'
            referencedColumns: ['id']
          },
        ]
      }
      post_likes: {
        Row: {
          id: string
          post_id: string
          user_id: string
          created_at: string | null
        }
        Insert: {
          id?: string
          post_id: string
          user_id: string
          created_at?: string | null
        }
        Update: {
          id?: string
          post_id?: string
          user_id?: string
          created_at?: string | null
        }
        Relationships: [
          {
            foreignKeyName: 'post_likes_post_id_fkey'
            columns: ['post_id']
            isOneToOne: false
            referencedRelation: 'posts'
            referencedColumns: ['id']
          },
        ]
      }
      comments: {
        Row: {
          id: string
          post_id: string
          author_id: string
          content: string
          parent_id: string | null
          created_at: string | null
          updated_at: string | null
        }
        Insert: {
          id?: string
          post_id: string
          author_id: string
          content: string
          parent_id?: string | null
          created_at?: string | null
          updated_at?: string | null
        }
        Update: {
          id?: string
          post_id?: string
          author_id?: string
          content?: string
          parent_id?: string | null
          created_at?: string | null
          updated_at?: string | null
        }
        Relationships: [
          {
            foreignKeyName: 'comments_author_id_fkey'
            columns: ['author_id']
            isOneToOne: false
            referencedRelation: 'profiles'
            referencedColumns: ['id']
          },
          {
            foreignKeyName: 'comments_parent_id_fkey'
            columns: ['parent_id']
            isOneToOne: false
            referencedRelation: 'comments'
            referencedColumns: ['id']
          },
          {
            foreignKeyName: 'comments_post_id_fkey'
            columns: ['post_id']
            isOneToOne: false
            referencedRelation: 'posts'
            referencedColumns: ['id']
          },
        ]
      }
      marketplace: {
        Row: {
          id: string
          seller_id: string
          title: string
          description: string
          category: string | null
          price: number
          location: string | null
          images: string[] | null
          status: string | null
          views_count: number | null
          likes_count: number | null
          created_at: string | null
          updated_at: string | null
        }
        Insert: {
          id?: string
          seller_id: string
          title: string
          description: string
          category?: string | null
          price: number
          location?: string | null
          images?: string[] | null
          status?: string | null
          views_count?: number | null
          likes_count?: number | null
          created_at?: string | null
          updated_at?: string | null
        }
        Update: {
          id?: string
          seller_id?: string
          title?: string
          description?: string
          category?: string | null
          price?: number
          location?: string | null
          images?: string[] | null
          status?: string | null
          views_count?: number | null
          likes_count?: number | null
          created_at?: string | null
          updated_at?: string | null
        }
        Relationships: [
          {
            foreignKeyName: 'marketplace_seller_id_fkey'
            columns: ['seller_id']
            isOneToOne: false
            referencedRelation: 'profiles'
            referencedColumns: ['id']
          },
        ]
      }
      recordings: {
        Row: {
          id: string
          user_id: string
          room_id: string | null
          title: string
          description: string | null
          file_url: string
          file_size: number | null
          duration: number | null
          format: string | null
          is_public: boolean | null
          created_at: string | null
        }
        Insert: {
          id?: string
          user_id: string
          room_id?: string | null
          title: string
          description?: string | null
          file_url: string
          file_size?: number | null
          duration?: number | null
          format?: string | null
          is_public?: boolean | null
          created_at?: string | null
        }
        Update: {
          id?: string
          user_id?: string
          room_id?: string | null
          title?: string
          description?: string | null
          file_url?: string
          file_size?: number | null
          duration?: number | null
          format?: string | null
          is_public?: boolean | null
          created_at?: string | null
        }
        Relationships: [
          {
            foreignKeyName: 'recordings_room_id_fkey'
            columns: ['room_id']
            isOneToOne: false
            referencedRelation: 'rooms'
            referencedColumns: ['id']
          },
          {
            foreignKeyName: 'recordings_user_id_fkey'
            columns: ['user_id']
            isOneToOne: false
            referencedRelation: 'profiles'
            referencedColumns: ['id']
          },
        ]
      }
      messages: {
        Row: {
          id: string
          sender_id: string
          receiver_id: string
          content: string
          is_read: boolean | null
          created_at: string | null
        }
        Insert: {
          id?: string
          sender_id: string
          receiver_id: string
          content: string
          is_read?: boolean | null
          created_at?: string | null
        }
        Update: {
          id?: string
          sender_id?: string
          receiver_id?: string
          content?: string
          is_read?: boolean | null
          created_at?: string | null
        }
        Relationships: [
          {
            foreignKeyName: 'messages_receiver_id_fkey'
            columns: ['receiver_id']
            isOneToOne: false
            referencedRelation: 'profiles'
            referencedColumns: ['id']
          },
          {
            foreignKeyName: 'messages_sender_id_fkey'
            columns: ['sender_id']
            isOneToOne: false
            referencedRelation: 'profiles'
            referencedColumns: ['id']
          },
        ]
      }
      stem_separations: {
        Row: {
          id: string
          user_id: string | null
          original_filename: string
          original_file_size: number | null
          vocals_url: string | null
          drums_url: string | null
          bass_url: string | null
          piano_url: string | null
          guitar_url: string | null
          other_url: string | null
          storage_folder: string
          status: string | null
          created_at: string | null
          expires_at: string | null
        }
        Insert: {
          id?: string
          user_id?: string | null
          original_filename: string
          original_file_size?: number | null
          vocals_url?: string | null
          drums_url?: string | null
          bass_url?: string | null
          piano_url?: string | null
          guitar_url?: string | null
          other_url?: string | null
          storage_folder: string
          status?: string | null
          created_at?: string | null
          expires_at?: string | null
        }
        Update: {
          id?: string
          user_id?: string | null
          original_filename?: string
          original_file_size?: number | null
          vocals_url?: string | null
          drums_url?: string | null
          bass_url?: string | null
          piano_url?: string | null
          guitar_url?: string | null
          other_url?: string | null
          storage_folder?: string
          status?: string | null
          created_at?: string | null
          expires_at?: string | null
        }
        Relationships: [
          {
            foreignKeyName: 'stem_separations_user_id_fkey'
            columns: ['user_id']
            isOneToOne: false
            referencedRelation: 'profiles'
            referencedColumns: ['id']
          },
        ]
      }
      subscriptions: {
        Row: {
          id: string
          user_id: string
          plan_type: string
          status: string
          amount: number
          payment_key: string | null
          start_date: string
          end_date: string
          cancelled_at: string | null
          created_at: string
          updated_at: string
        }
        Insert: {
          id?: string
          user_id: string
          plan_type: string
          status?: string
          amount: number
          payment_key?: string | null
          start_date?: string
          end_date: string
          cancelled_at?: string | null
          created_at?: string
          updated_at?: string
        }
        Update: {
          id?: string
          user_id?: string
          plan_type?: string
          status?: string
          amount?: number
          payment_key?: string | null
          start_date?: string
          end_date?: string
          cancelled_at?: string | null
          created_at?: string
          updated_at?: string
        }
        Relationships: []
      }
      payments: {
        Row: {
          id: string
          user_id: string
          order_id: string
          payment_key: string
          plan_type: string
          amount: number
          status: string
          subscription_id: string | null
          cancel_reason: string | null
          cancelled_at: string | null
          created_at: string
          updated_at: string
        }
        Insert: {
          id?: string
          user_id: string
          order_id: string
          payment_key: string
          plan_type: string
          amount: number
          status?: string
          subscription_id?: string | null
          cancel_reason?: string | null
          cancelled_at?: string | null
          created_at?: string
          updated_at?: string
        }
        Update: {
          id?: string
          user_id?: string
          order_id?: string
          payment_key?: string
          plan_type?: string
          amount?: number
          status?: string
          subscription_id?: string | null
          cancel_reason?: string | null
          cancelled_at?: string | null
          created_at?: string
          updated_at?: string
        }
        Relationships: [
          {
            foreignKeyName: 'payments_subscription_id_fkey'
            columns: ['subscription_id']
            isOneToOne: false
            referencedRelation: 'subscriptions'
            referencedColumns: ['id']
          },
        ]
      }
      payment_orders: {
        Row: {
          order_id: string
          user_id: string
          plan_type: string
          amount: number
          expires_at: string
          confirm_attempted_at: string | null
          created_at: string
        }
        Insert: {
          order_id: string
          user_id: string
          plan_type: string
          amount: number
          expires_at: string
          confirm_attempted_at?: string | null
          created_at?: string
        }
        Update: {
          order_id?: string
          user_id?: string
          plan_type?: string
          amount?: number
          expires_at?: string
          confirm_attempted_at?: string | null
          created_at?: string
        }
        Relationships: []
      }
    }
    Views: {
      [_ in never]: never
    }
    Functions: {
      increment_participants: {
        Args: { room_id: string }
        Returns: undefined
      }
      decrement_participants: {
        Args: { room_id: string }
        Returns: undefined
      }
      cleanup_expired_stem_separations: {
        Args: Record<PropertyKey, never>
        Returns: number
      }
      get_room: {
        Args: { p_room_id: string }
        Returns: Database['public']['Tables']['rooms']['Row'][]
        SetofOptions: {
          from: '*'
          to: 'rooms'
          isOneToOne: false
          isSetofReturn: true
        }
      }
      set_room_password: {
        Args: { p_room_id: string; p_hash: string | null }
        Returns: undefined
      }
      record_confirmed_payment: {
        Args: {
          p_user_id: string
          p_order_id: string
          p_payment_key: string
          p_plan_type: string
          p_amount: number
          p_period_days: number
        }
        Returns: Database['public']['Tables']['subscriptions']['Row']
        SetofOptions: {
          from: '*'
          to: 'subscriptions'
          isOneToOne: true
          isSetofReturn: false
        }
      }
      cancel_subscription: {
        Args: { p_subscription_id: string; p_cancel_payment: boolean; p_reason: string }
        Returns: boolean
      }
    }
    Enums: {
      [_ in never]: never
    }
    CompositeTypes: {
      [_ in never]: never
    }
  }
}

type DatabaseWithoutInternals = Omit<Database, '__InternalSupabase'>
type DefaultSchema = DatabaseWithoutInternals[Extract<keyof Database, 'public'>]

/** 테이블 행 타입. 예: Tables<'posts'> */
export type Tables<T extends keyof DefaultSchema['Tables']> = DefaultSchema['Tables'][T]['Row']
/** insert 페이로드 타입 */
export type TablesInsert<T extends keyof DefaultSchema['Tables']> = DefaultSchema['Tables'][T]['Insert']
/** update 페이로드 타입 */
export type TablesUpdate<T extends keyof DefaultSchema['Tables']> = DefaultSchema['Tables'][T]['Update']
/** RPC 반환 타입 */
export type FunctionReturns<T extends keyof DefaultSchema['Functions']> = DefaultSchema['Functions'][T]['Returns']
