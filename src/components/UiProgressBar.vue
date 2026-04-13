<script setup lang="ts">
defineProps<{
  value: number;
  color?: string;
  height?: number;
  caption?: string;
}>();
</script>

<template>
  <div>
    <v-progress-linear
      :model-value="Math.min(100, Math.max(0, value))"
      :color="color ?? 'primary'"
      rounded
      :height="height ?? 10"
    />
    <div v-if="caption" class="caption mt-1">{{ caption }}</div>
  </div>
</template>

<style scoped>
.caption {
  font-size: 12px;
  color: #4b5563;
}
</style>
