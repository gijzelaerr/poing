use makepad_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

script_mod! {
    use mod.prelude.widgets.*

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                pass.clear_color: vec4(0.08, 0.08, 0.12, 1.0)
                window.inner_size: vec2(800, 600)
                body +: {
                    width: Fill height: Fill
                    flow: Down
                    align: Center
                    spacing: 20

                    Label{
                        text: "Poing"
                        draw_text.color: #xfff
                        draw_text.text_style.font_size: 32
                    }

                    Label{
                        text: "Neural Audio Generation"
                        draw_text.color: #x888
                        draw_text.text_style.font_size: 14
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
}

impl App {
    pub fn run(vm: &mut ScriptVm) -> Self {
        makepad_widgets::script_mod(vm);
        App::from_script_mod(vm, self::script_mod)
    }
}

impl MatchEvent for App {
    fn handle_actions(&mut self, _cx: &mut Cx, _actions: &Actions) {}
}

impl AppMain for App {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        cx.with_widget_tree(|cx| {
            self.match_event(cx, event);
            self.ui.handle_event(cx, event, &mut Scope::empty());
        });
    }
}

/// Create the Makepad event handler closure for embedding.
/// This is called by poing-makepad-bridge to set up the Cx.
pub fn create_event_handler() -> Box<dyn FnMut(&mut Cx, &Event)> {
    let app: Rc<RefCell<Option<App>>> = Rc::new(RefCell::new(None));
    Box::new(move |cx, event| {
        if let Event::Startup = event {
            *app.borrow_mut() = Some(cx.with_vm(|vm| App::run(vm)));
        }
        if let Some(app) = &mut *app.borrow_mut() {
            <dyn AppMain>::handle_event(app, cx, event);
        }
    })
}
