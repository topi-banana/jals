struct App {}

impl yew::Component for App {
    type Message = ();
    type Properties = ();
    fn create(_ctx: &yew::Context<Self>) -> Self {
        Self {}
    }
    fn view(&self, _ctx: &yew::Context<Self>) -> yew::Html {
        yew::html! {}
    }
}

fn main() {
    yew::Renderer::<App>::new().render();
}
