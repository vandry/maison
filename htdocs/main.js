const {HelloRequest, HelloResponse} = require('codegen/maison_pb.js');
const {MaisonClient} = require('codegen/maison_grpc_web_pb.js');

class Maison {
    constructor (api) {
        this.api = api;
        this.output_el = document.getElementById("output");
    }

    run = () => {
        let req = new HelloRequest();
        req.setMessage("zigbee/boiler");
        var output_el = this.output_el;
        var stream = this.api.mqttTest(req, {});
        stream.on('data', function(response) {
            var d = document.createElement('div');
            d.textContent = response.getMessage();
            output_el.appendChild(d);
        });
        stream.on('status', function(status) {
            var d = document.createElement('div');
            var f = document.createElement('font');
            f.color = 'red';
            f.textContent = status.code;
            d.appendChild(f);
            output_el.appendChild(d);
        });
        stream.on('end', function() {
            var d = document.createElement('div');
            var f = document.createElement('font');
            f.color = 'green';
            f.textContent = 'END';
            d.appendChild(f);
            output_el.appendChild(d);
        });
    }
}

new Maison(new MaisonClient(URL.parse(window.location).toString())).run();
