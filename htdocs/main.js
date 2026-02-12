const {HelloRequest, HelloResponse} = require('codegen/maison_pb.js');
const {MaisonClient} = require('codegen/maison_grpc_web_pb.js');

class Maison {
    constructor (api) {
        this.api = api;
        this.output_el = document.getElementById("output");
    }

    run = () => {
        let req = new HelloRequest();
        req.setMessage("test");
        this.api.hello(req, {}, (err, response) => {
            if (err === null) {
                this.output_el.textContent = response.getMessage();
            } else {
                this.output_el.textContent = err;
            }
        });
    }
}

new Maison(new MaisonClient(URL.parse(window.location).toString())).run();
