<script setup lang="ts">
import { computed } from "vue";

const props = defineProps<{
  label: string;
  kind?: "success" | "warning" | "error" | "info" | "neutral";
}>();

const color = computed(() => {
  switch (props.kind) {
    case "success":
      return "secondary";
    case "warning":
      return "warning";
    case "error":
      return "error";
    case "info":
      return "info";
    default:
      return "grey";
  }
});
</script>

<template>
  <v-chip :color="color" size="small" variant="flat">
    {{ label }}
  </v-chip>
</template>
