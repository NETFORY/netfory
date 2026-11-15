<script setup lang="ts">
/* eslint-disable no-undef */
// Аватар пользователя: мгновенно из кэша (store/users), фолбэк — Identicon.
import { watch } from 'vue'
import Identicon from '@/components/Identicon.vue'
import { useUsersStore } from '@/store/users'

const props = defineProps<{ address: string; size: number }>()
const users = useUsersStore()

watch(() => props.address, (a) => users.ensure(a), { immediate: true })
</script>

<template>
  <span
    class="uav"
    :style="{ width: size + 'px', height: size + 'px' }"
    data-testid="user-avatar"
  >
    <img
      v-if="users.avatars[address]"
      :src="users.avatars[address] as string"
      alt=""
      class="uav-img"
    />
    <Identicon v-else :seed="address" :size="size" />
  </span>
</template>

<style scoped>
.uav { display: inline-flex; border-radius: 50%; overflow: hidden; flex: 0 0 auto; }
.uav-img { width: 100%; height: 100%; object-fit: cover; display: block; }
</style>
