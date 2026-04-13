<script setup lang="ts">
defineProps<{
  text: string;
  kind?: "success" | "warning" | "error" | "info";
}>();
</script>

<template>
  <v-alert
    :type="kind ?? 'info'"
    variant="tonal"
    density="comfortable"
    class="mb-4"
  >
    {{ text }}
  </v-alert>
</template>
