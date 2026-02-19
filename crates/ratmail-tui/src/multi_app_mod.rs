use super::{App, MultiApp};

impl MultiApp {
    pub(crate) fn new(apps: Vec<App>) -> Self {
        Self { apps, current: 0 }
    }

    pub(crate) fn current(&self) -> &App {
        &self.apps[self.current]
    }

    pub(crate) fn current_mut(&mut self) -> &mut App {
        &mut self.apps[self.current]
    }

    pub(crate) fn switch_next(&mut self) {
        if !self.apps.is_empty() {
            self.current = (self.current + 1) % self.apps.len();
        }
    }

    pub(crate) fn switch_prev(&mut self) {
        if !self.apps.is_empty() {
            if self.current == 0 {
                self.current = self.apps.len() - 1;
            } else {
                self.current -= 1;
            }
        }
    }

    pub(crate) fn account_labels(&self) -> Vec<String> {
        self.apps
            .iter()
            .map(|app| app.store.account.name.clone())
            .collect()
    }

    pub(crate) fn drain_all(&mut self) {
        for app in &mut self.apps {
            app.drain_channels();
        }
    }

    pub(crate) fn tick_all(&mut self) {
        for app in &mut self.apps {
            app.on_tick();
        }
    }
}
