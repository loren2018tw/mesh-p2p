import { createApp } from "vue";
import { createVuetify } from "vuetify";
import * as components from "vuetify/components";
import * as directives from "vuetify/directives";
import { aliases, mdi } from "vuetify/iconsets/mdi";
import "vuetify/styles";
import "@mdi/font/css/materialdesignicons.css";
import App from "./App.vue";

const vuetify = createVuetify({
  components,
  directives,
  icons: {
    defaultSet: "mdi",
    aliases,
    sets: { mdi },
  },
  theme: {
    defaultTheme: "mesh",
    themes: {
      mesh: {
        dark: false,
        colors: {
          primary: "#0f766e",
          secondary: "#14532d",
          accent: "#c2410c",
          info: "#0369a1",
          warning: "#a16207",
          background: "#f5f7f2",
          surface: "#ffffff",
        },
      },
    },
  },
});

createApp(App).use(vuetify).mount("#app");
